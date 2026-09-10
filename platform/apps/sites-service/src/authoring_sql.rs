//! One bounded authoring SQL statement: reads, DDL, DML and RETURNING.
//! Mutation results and retry receipts commit in the same transaction.
use crate::{Error, Result, SiteService, database};
use platform_runtime_contracts::sites_authoring::Sql;
use rusqlite::{
    Connection, OptionalExtension,
    hooks::{AuthContext, Authorization},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;

impl SiteService {
    pub async fn sql_statement(&self, site: Uuid, id: Uuid, input: Sql) -> Result<Value> {
        if id.is_nil()
            || input.sql.len() > 48 * 1024
            || input.params.len() > 256
            || !database::single_statement(&input.sql)
        {
            return Err(Error::Invalid(
                "Supply one bounded SQL statement and a stable operation ID.",
            ));
        }
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _gate = service.site_gate(site).await?;
            service.require_ready(site).await?;
            let record = service.get(site).await?;
            let project = record.project_id.parse().map_err(|_| Error::Storage)?;
            let root = service.0.storage.root.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn = database::open(&root, site, project)?;
                execute(&mut conn, id, input)
            })
            .await?
        })
        .await?
    }
}

fn execute(conn: &mut Connection, id: Uuid, input: Sql) -> Result<Value> {
    if id.is_nil()
        || input.sql.len() > 48 * 1024
        || input.params.len() > 256
        || !database::single_statement(&input.sql)
    {
        return Err(Error::Invalid("Supply one bounded SQL statement."));
    }
    let invalid = || {
        Error::Invalid(
            "SQL was rejected, failed, or exceeded limits. Use one statement, unique column aliases, safe scalar parameters and bounded results.",
        )
    };
    let params = input
        .params
        .iter()
        .cloned()
        .map(database::parameter)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    let request = serde_json::to_string(&input).map_err(|_| Error::Storage)?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS __sites_authoring_receipts(id TEXT PRIMARY KEY,request TEXT NOT NULL,response TEXT NOT NULL)")
        .map_err(|_| Error::Storage)?;
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)?;
    let saved: Option<(String, String)> = tx
        .query_row(
            "SELECT request,response FROM __sites_authoring_receipts WHERE id=?",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    if let Some((old, response)) = saved {
        if old != request {
            return Err(Error::Conflict(
                "IDEMPOTENCY_CONFLICT",
                "SQL operation input changed.",
            ));
        }
        return serde_json::from_str(&response).map_err(|_| Error::Storage);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    tx.progress_handler(1000, Some(move || Instant::now() >= deadline));
    tx.authorizer(Some(|ctx: AuthContext<'_>| database::authorize(ctx, true)));
    let before = tx.total_changes();
    let result = (|| {
        let mut stmt = tx.prepare(&input.sql).map_err(|_| invalid())?;
        let read_only = stmt.readonly();
        let names: Vec<String> = stmt.column_names().into_iter().map(str::to_owned).collect();
        let mut unique = std::collections::HashSet::new();
        if names.iter().any(|n| !unique.insert(n)) {
            return Err(invalid());
        }
        let mut cursor = stmt
            .query(rusqlite::params_from_iter(params))
            .map_err(|_| invalid())?;
        let mut rows = Vec::new();
        let mut bytes = 0;
        while let Some(row) = cursor.next().map_err(|_| invalid())? {
            if Instant::now() >= deadline || rows.len() >= 1000 {
                return Err(invalid());
            }
            let mut value = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                value.insert(
                    name.clone(),
                    database::value(row.get_ref(i).map_err(|_| invalid())?)
                        .map_err(|_| invalid())?,
                );
            }
            bytes += serde_json::to_vec(&value)
                .map_err(|_| Error::Storage)?
                .len();
            if bytes > 95 * 1024 {
                return Err(invalid());
            }
            rows.push(value);
        }
        drop(cursor);
        drop(stmt);
        let changes = if read_only || tx.total_changes() == before {
            0
        } else {
            tx.changes()
        };
        let value = json!({"rows":rows,"changes":changes,"read_only":read_only});
        if value.to_string().len() > 96 * 1024 {
            return Err(invalid());
        }
        Ok((value, read_only))
    })();
    // Remove hooks before rollback, receipt writes, or commit, including errors.
    tx.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
    tx.progress_handler(0, None::<fn() -> bool>);
    let (value, read_only) = result?;
    let reserved: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE lower(name) GLOB '__sites_*' AND name NOT IN ('__sites_identity','__sites_migrations','__sites_authoring_receipts')",
        [], |r| r.get(0)
    ).map_err(|_| Error::Storage)?;
    if reserved != 0 {
        return Err(Error::Invalid("Reserved schema names cannot be used."));
    }
    if !read_only {
        tx.execute(
            "INSERT INTO __sites_authoring_receipts VALUES(?,?,?)",
            rusqlite::params![id.to_string(), request, value.to_string()],
        )
        .map_err(|_| Error::Storage)?;
    }
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(conn: &mut Connection, id: Uuid, sql: &str, params: Vec<Value>) -> Result<Value> {
        execute(
            conn,
            id,
            Sql {
                sql: sql.into(),
                params,
            },
        )
    }
    #[test]
    fn authoring_indexes_populate_replay_and_drop_without_backend_ddl() {
        let mut conn = Connection::open_in_memory().unwrap();
        call(
            &mut conn,
            Uuid::now_v7(),
            "CREATE TABLE items(id INTEGER PRIMARY KEY, title TEXT)",
            vec![],
        )
        .unwrap();
        call(
            &mut conn,
            Uuid::now_v7(),
            "INSERT INTO items VALUES(1,'first'),(2,'second')",
            vec![],
        )
        .unwrap();
        let id = Uuid::now_v7();
        let sql = "CREATE INDEX IF NOT EXISTS items_title_idx ON items(title DESC, id DESC)";
        let result = call(&mut conn, id, sql, vec![]).unwrap();
        assert_eq!(result, json!({"rows":[],"changes":0,"read_only":false}));
        assert_eq!(call(&mut conn, id, sql, vec![]).unwrap(), result);
        assert_eq!(
            call(
                &mut conn,
                Uuid::now_v7(),
                "SELECT id FROM items INDEXED BY items_title_idx ORDER BY title DESC",
                vec![]
            )
            .unwrap()["rows"],
            json!([{"id":2},{"id":1}])
        );
        // Runtime DB access must not acquire the authoring permission.
        conn.authorizer(Some(|ctx: AuthContext<'_>| database::authorize(ctx, false)));
        assert!(conn.execute_batch("REINDEX items_title_idx").is_err());
        conn.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
        call(
            &mut conn,
            Uuid::now_v7(),
            "DROP INDEX items_title_idx",
            vec![],
        )
        .unwrap();
        call(
            &mut conn,
            Uuid::now_v7(),
            "CREATE UNIQUE INDEX items_title_unique ON items(title)",
            vec![],
        )
        .unwrap();
    }
    #[test]
    fn unified_sql_schema_reads_writes_returning_and_receipts() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            call(
                &mut conn,
                Uuid::now_v7(),
                "CREATE TABLE items(id INTEGER PRIMARY KEY, name TEXT)",
                vec![]
            )
            .unwrap(),
            json!({"rows":[],"changes":0,"read_only":false})
        );
        let id = Uuid::now_v7();
        let write = "INSERT INTO items(name) VALUES (?) RETURNING id,name";
        let params = vec![json!("example")];
        let result = call(&mut conn, id, write, params.clone()).unwrap();
        assert_eq!(
            result,
            json!({"rows":[{"id":1,"name":"example"}],"changes":1,"read_only":false})
        );
        assert_eq!(call(&mut conn, id, write, params).unwrap(), result);
        assert!(call(&mut conn, id, write, vec![json!("different")]).is_err());
        let read_id = Uuid::now_v7();
        let sql = "SELECT count(*) AS count FROM items";
        assert_eq!(
            call(&mut conn, read_id, sql, vec![]).unwrap(),
            json!({"rows":[{"count":1}],"changes":0,"read_only":true})
        );
        call(
            &mut conn,
            Uuid::now_v7(),
            "INSERT INTO items(name) VALUES ('second')",
            vec![],
        )
        .unwrap();
        assert_eq!(
            call(&mut conn, read_id, sql, vec![]).unwrap()["rows"],
            json!([{"count":2}])
        );
        assert_eq!(
            call(
                &mut conn,
                Uuid::now_v7(),
                "SELECT name FROM sqlite_schema WHERE name='items'",
                vec![]
            )
            .unwrap()["rows"],
            json!([{"name":"items"}])
        );
        // DDL must not report the preceding insert's changes().
        assert_eq!(
            call(
                &mut conn,
                Uuid::now_v7(),
                "ALTER TABLE items ADD COLUMN note TEXT",
                vec![]
            )
            .unwrap()["changes"],
            0
        );
    }
    #[test]
    fn rejected_or_oversized_mutations_roll_back_and_release_hooks() {
        let mut conn = Connection::open_in_memory().unwrap();
        call(
            &mut conn,
            Uuid::now_v7(),
            "CREATE TABLE items(id INTEGER)",
            vec![],
        )
        .unwrap();
        for sql in [
            "SELECT * FROM __sites_authoring_receipts",
            "DROP TABLE __sites_authoring_receipts",
            "CREATE TABLE __sites_private(id INTEGER)",
            "ATTACH ':memory:' AS other",
            "PRAGMA writable_schema=ON",
            "SELECT load_extension('unavailable')",
            "INSERT INTO items VALUES(1); INSERT INTO items VALUES(2)",
            "INSERT INTO items VALUES(1) RETURNING id AS duplicate,id AS duplicate",
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO items SELECT x FROM n RETURNING id",
            "INSERT INTO items VALUES(1) RETURNING hex(zeroblob(50000)) AS big",
        ] {
            assert!(
                call(&mut conn, Uuid::now_v7(), sql, vec![]).is_err(),
                "{sql}"
            );
            assert_eq!(
                call(
                    &mut conn,
                    Uuid::now_v7(),
                    "SELECT count(*) AS count FROM items",
                    vec![]
                )
                .unwrap()["rows"],
                json!([{"count":0}]),
                "{sql}"
            );
        }
        assert!(call(&mut conn, Uuid::now_v7(), "SELECT ?", vec![json!(true)]).is_err());
        assert!(conn.is_autocommit());
        call(
            &mut conn,
            Uuid::now_v7(),
            "INSERT INTO items VALUES(7)",
            vec![],
        )
        .unwrap();
    }
}
