//! Trusted SQLite owner. Guest code only supplies bounded SQL and values, never paths.
use crate::{Error, Result, storage};
use rusqlite::{
    Connection, OpenFlags,
    hooks::{AuthAction as A, AuthContext, Authorization},
    limits::Limit,
    types::{Value as SqlValue, ValueRef},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MAX_RESULT: usize = 256 * 1024;
pub const MAX_ROWS: usize = 1000;
pub const MAX_SQL: usize = 64 * 1024;
const SAFE_INTEGER: i64 = 9_007_199_254_740_991;

#[derive(Debug, Serialize, Clone)]
pub struct DbError {
    pub code: &'static str,
    pub message: &'static str,
}
impl DbError {
    pub fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}
impl From<rusqlite::Error> for DbError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("SQL_ERROR", "SQL was rejected or could not be executed.")
    }
}
type DbResult<T> = std::result::Result<T, DbError>;
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    pub sql: String,
    #[serde(default)]
    pub params: Vec<Value>,
}

pub struct Database {
    pub(crate) conn: Connection,
    transaction: Option<Instant>,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

pub(crate) fn path(root: &Path, site: Uuid) -> Result<PathBuf> {
    storage::existing_directory(root)?;
    storage::existing_directory(&root.join("sites"))?;
    let dir = root.join("sites").join(site.to_string());
    storage::existing_directory(&dir)?;
    storage::existing_directory(&dir.join("data"))?;
    let path = dir.join("data/site.sqlite");
    storage::sqlite_file(&path, false)?;
    Ok(path)
}

pub(crate) fn open(root: &Path, site: Uuid, project: Uuid) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path(root, site)?,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| Error::Storage)?;
    conn.busy_timeout(Duration::from_millis(250))
        .map_err(|_| Error::Storage)?;
    conn.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;",
    )
    .map_err(|_| Error::Storage)?;
    let identity: (String, String) = conn
        .query_row(
            "SELECT site_id,project_id FROM __sites_identity WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|_| Error::Storage)?;
    if identity != (site.to_string(), project.to_string()) {
        return Err(Error::Storage);
    }
    conn.execute_batch("CREATE TABLE IF NOT EXISTS __sites_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL);")
        .map_err(|_| Error::Storage)?;
    let page_size: u32 = conn
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    // Per-site database growth bound; existing larger databases can still be read/deleted.
    conn.pragma_update(None, "max_page_count", (64 * 1024 * 1024) / page_size)
        .map_err(|_| Error::Storage)?;
    for (limit, value) in [
        (Limit::SQLITE_LIMIT_LENGTH, MAX_RESULT as i32),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL as i32),
        (Limit::SQLITE_LIMIT_COLUMN, 100),
        (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 256),
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
        (Limit::SQLITE_LIMIT_VDBE_OP, 100_000),
        (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 16),
    ] {
        conn.set_limit(limit, value);
    }
    Ok(conn)
}

pub(crate) fn version(conn: &Connection) -> Result<u32> {
    conn.query_row(
        "SELECT COALESCE(MAX(version),0) FROM __sites_migrations",
        [],
        |r| r.get(0),
    )
    .map_err(|_| Error::Storage)
}
fn protected(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("__sites_")
}
fn application(name: &str) -> bool {
    !protected(name) && !name.to_ascii_lowercase().starts_with("sqlite_")
}

pub(crate) fn authorize(ctx: AuthContext<'_>, migrations: bool) -> Authorization {
    let allowed = match ctx.action {
        A::Select | A::Recursive => true,
        A::Read { table_name, .. } => {
            application(table_name) || (migrations && !protected(table_name))
        }
        A::Insert { table_name } | A::Update { table_name, .. } | A::Delete { table_name } => {
            application(table_name) || (migrations && !protected(table_name))
        }
        A::Function { function_name } => {
            let n = function_name.to_ascii_lowercase();
            !n.starts_with("pragma_")
                && !["load_extension", "readfile", "writefile"].contains(&n.as_str())
        }
        A::CreateTable { table_name }
        | A::DropTable { table_name }
        | A::AlterTable { table_name, .. } => migrations && application(table_name),
        A::CreateIndex {
            index_name,
            table_name,
        }
        | A::DropIndex {
            index_name,
            table_name,
        } => migrations && application(table_name) && !protected(index_name),
        A::CreateView { view_name } | A::DropView { view_name } => {
            migrations && application(view_name)
        }
        A::CreateTrigger {
            trigger_name,
            table_name,
        }
        | A::DropTrigger {
            trigger_name,
            table_name,
        } => migrations && application(table_name) && application(trigger_name),
        _ => false,
    };
    if allowed {
        Authorization::Allow
    } else {
        Authorization::Deny
    }
}
fn hooks(conn: &Connection, migrations: bool) {
    conn.authorizer(Some(move |ctx: AuthContext<'_>| authorize(ctx, migrations)));
}
fn unhook(conn: &Connection) {
    conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
}

impl Database {
    pub fn new(conn: Connection, deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            conn,
            transaction: None,
            deadline,
            cancelled,
        }
    }
    fn check(&self) -> DbResult<()> {
        if self.cancelled.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            return Err(DbError::new(
                "INVOCATION_CANCELLED",
                "Invocation deadline or cancellation reached.",
            ));
        }
        if self
            .transaction
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            return Err(DbError::new(
                "TRANSACTION_TIMEOUT",
                "Transaction exceeded two seconds.",
            ));
        }
        Ok(())
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }
    pub fn rollback(&mut self) {
        unhook(&self.conn);
        self.conn.progress_handler(0, None::<fn() -> bool>);
        let _ = self.conn.execute_batch("ROLLBACK");
        self.transaction = None;
    }
    fn begin(&mut self) -> DbResult<()> {
        self.check()?;
        if self.transaction.is_some() {
            return Err(DbError::new(
                "TRANSACTION_ACTIVE",
                "Nested transactions are not supported.",
            ));
        }
        unhook(&self.conn);
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.transaction = Some(Instant::now());
        Ok(())
    }
    fn commit(&mut self) -> DbResult<()> {
        self.check()?;
        if self.transaction.is_none() {
            return Err(DbError::new("NO_TRANSACTION", "No active transaction."));
        }
        unhook(&self.conn);
        self.conn.execute_batch("COMMIT")?;
        self.transaction = None;
        Ok(())
    }
    pub fn call(&mut self, method: &str, args: Value) -> DbResult<Value> {
        // Rollback must work even after the deadline has passed.
        if method == "rollback" {
            self.rollback();
            return Ok(Value::Null);
        }
        self.check()?;
        let result = match method {
            "check" => Ok(Value::Null),
            "query" | "execute" => {
                let statement = serde_json::from_value(args).map_err(|_| {
                    DbError::new(
                        "INVALID_SQL_INPUT",
                        "Expected SQL and positional parameters.",
                    )
                })?;
                self.statement(statement, method == "query")
            }
            "begin" => self.begin().map(|_| Value::Null),
            "commit" => self.commit().map(|_| Value::Null),
            "batch" => {
                let statements: Vec<Statement> = serde_json::from_value(args).map_err(|_| {
                    DbError::new("INVALID_SQL_INPUT", "Expected a statement array.")
                })?;
                if statements.is_empty() || statements.len() > 100 {
                    return Err(DbError::new(
                        "SQL_LIMIT",
                        "Batch requires 1–100 statements.",
                    ));
                }
                self.begin()?;
                let result = (|| {
                    let mut values = Vec::new();
                    for statement in statements {
                        values.push(self.statement(statement, false)?);
                    }
                    let result = json!(values);
                    if serde_json::to_vec(&result).unwrap().len() > MAX_RESULT {
                        return Err(DbError::new("RESULT_LIMIT", "Batch result is too large."));
                    }
                    self.commit()?;
                    Ok(result)
                })();
                if result.is_err() {
                    self.rollback();
                }
                result
            }
            _ => Err(DbError::new(
                "UNKNOWN_CAPABILITY",
                "Unknown database capability.",
            )),
        };
        if result.is_err() && self.check().is_err() {
            self.rollback();
        }
        result
    }
    fn statement(&mut self, input: Statement, read_only: bool) -> DbResult<Value> {
        if input.sql.len() > MAX_SQL || input.params.len() > 256 || !single_statement(&input.sql) {
            return Err(DbError::new(
                "INVALID_SQL_INPUT",
                "Supply one bounded SQL statement.",
            ));
        }
        let params = input
            .params
            .into_iter()
            .map(parameter)
            .collect::<DbResult<Vec<_>>>()?;
        let query_deadline = (Instant::now() + Duration::from_secs(1))
            .min(self.deadline)
            .min(
                self.transaction
                    .map(|t| t + Duration::from_secs(2))
                    .unwrap_or(self.deadline),
            );
        let cancelled = self.cancelled.clone();
        self.conn.progress_handler(
            1000,
            Some(move || Instant::now() >= query_deadline || cancelled.load(Ordering::Relaxed)),
        );
        hooks(&self.conn, false);
        // A savepoint makes rejected/oversized RETURNING results roll back the
        // statement's writes, including when inside a caller-owned transaction.
        unhook(&self.conn);
        self.conn.execute_batch("SAVEPOINT sdk_statement")?;
        hooks(&self.conn, false);
        let result = (|| {
            let mut stmt = self.conn.prepare(&input.sql)?;
            if read_only && !stmt.readonly() {
                return Err(DbError::new(
                    "READ_ONLY_QUERY",
                    "Use execute for mutations.",
                ));
            }
            let readonly = stmt.readonly();
            let names = stmt
                .column_names()
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut unique = std::collections::HashSet::new();
            if names.iter().any(|n| !unique.insert(n)) {
                return Err(DbError::new(
                    "DUPLICATE_COLUMN",
                    "Use unique column aliases.",
                ));
            }
            let mut cursor = stmt.query(rusqlite::params_from_iter(params))?;
            let mut output = Vec::new();
            let mut size = 0;
            while let Some(row) = cursor.next()? {
                self.check()?;
                let mut record = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    record.insert(name.clone(), value(row.get_ref(i)?)?);
                }
                size += serde_json::to_vec(&record).unwrap().len();
                if size > MAX_RESULT - 1024 || output.len() >= MAX_ROWS {
                    return Err(DbError::new(
                        "RESULT_LIMIT",
                        "Result exceeds 1000 rows or 256 KiB. Paginate explicitly.",
                    ));
                }
                output.push(Value::Object(record));
            }
            let changes = if readonly { 0 } else { self.conn.changes() };
            Ok(if read_only {
                json!(output)
            } else {
                json!({"changes":changes,"rows":output})
            })
        })();
        unhook(&self.conn);
        self.conn.progress_handler(0, None::<fn() -> bool>);
        match result {
            Ok(value) => {
                self.conn.execute_batch("RELEASE sdk_statement")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self
                    .conn
                    .execute_batch("ROLLBACK TO sdk_statement; RELEASE sdk_statement;");
                Err(error)
            }
        }
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        self.rollback();
    }
}

fn parameter(value: Value) -> DbResult<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::String(s) if s.len() <= MAX_RESULT => SqlValue::Text(s),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if !(-SAFE_INTEGER..=SAFE_INTEGER).contains(&i) {
                    return Err(DbError::new(
                        "SQL_VALUE",
                        "Integers must be JavaScript-safe; use TEXT or explicit CAST.",
                    ));
                }
                SqlValue::Integer(i)
            } else {
                let f = n
                    .as_f64()
                    .ok_or(DbError::new("SQL_VALUE", "Invalid numeric value."))?;
                if !f.is_finite() || f.abs() > SAFE_INTEGER as f64 {
                    return Err(DbError::new(
                        "SQL_VALUE",
                        "Numeric parameter is out of range.",
                    ));
                }
                SqlValue::Real(f)
            }
        }
        _ => {
            return Err(DbError::new(
                "SQL_VALUE",
                "Parameters support null, strings, and finite safe numbers only.",
            ));
        }
    })
}
fn value(input: ValueRef<'_>) -> DbResult<Value> {
    match input {
        ValueRef::Null => Ok(Value::Null),
        ValueRef::Integer(i) if (-SAFE_INTEGER..=SAFE_INTEGER).contains(&i) => Ok(json!(i)),
        ValueRef::Real(f) if f.is_finite() => Ok(json!(f)),
        ValueRef::Text(bytes) => Ok(Value::String(
            std::str::from_utf8(bytes)
                .map_err(|_| DbError::new("SQL_VALUE", "Invalid UTF-8."))?
                .into(),
        )),
        _ => Err(DbError::new(
            "SQL_VALUE",
            "Cast large integers/BLOBs to TEXT before reading.",
        )),
    }
}

// Recognize only a single statement; semicolons in quoted literals/comments do
// not delimit SQL. SQLite itself performs all actual parsing and authorization.
fn single_statement(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut i = 0;
    let mut ended = false;
    let mut content = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            i += 2;
            while i + 1 < bytes.len() && !bytes[i..].starts_with(b"*/") {
                i += 1;
            }
            if i + 1 >= bytes.len() {
                return false;
            }
            i += 2;
            continue;
        }
        if ended || c == 0 {
            return false;
        }
        if c == b';' {
            ended = true;
            i += 1;
            continue;
        }
        content = true;
        if b"'\"`[".contains(&c) {
            let end = if c == b'[' { b']' } else { c };
            i += 1;
            loop {
                if i >= bytes.len() {
                    return false;
                }
                if bytes[i] == end {
                    i += 1;
                    if end != b']' && i < bytes.len() && bytes[i] == end {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
        }
    }
    content
}
