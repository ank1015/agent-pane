//! Bounded authoring SQL. Writes and their receipts share a SQLite transaction.
use crate::{Error, Result, SiteService, Status, database};
use platform_runtime_contracts::sites_authoring::Sql;
use rusqlite::hooks::{AuthContext, Authorization};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use uuid::Uuid;
impl SiteService {
    pub async fn authoring_sql(&self, site: Uuid, id: Option<Uuid>, input: Sql) -> Result<Value> {
        if input.sql.len() > 48 * 1024 || input.params.len() > 256 {
            return Err(Error::Invalid("SQL exceeds authoring bounds."));
        }
        let permit = self
            .0
            .executions
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        let service = self.clone();
        tokio::spawn(async move {
            let _permit=permit;let _gate=service.site_gate(site).await?;
            let record=service.get(site).await?;
            if record.status!=Status::Ready {return Err(Error::Conflict("SITE_NOT_READY","Site is not ready."));}
            let project=Uuid::parse_str(&record.project_id).map_err(|_|Error::Storage)?;
            let root=service.0.storage.root.clone();
            tokio::task::spawn_blocking(move || {
                let mut conn=database::open(&root,site,project)?;
                let deadline=Instant::now()+Duration::from_secs(2);
                if id.is_none() {
                    let mut db=database::Database::new(conn,deadline,Arc::new(AtomicBool::new(false)));
                    let value=db.call("query",json!(input)).map_err(|_|Error::Invalid("Read-only SQL failed or exceeded limits."))?;
                    if value.to_string().len()>96*1024 {return Err(Error::Invalid("Query result exceeds 96 KiB; narrow the query."));}return Ok(value);
                }
                let id=id.unwrap();if id.is_nil(){return Err(Error::Invalid("Invalid SQL operation ID."));}
                conn.execute_batch("CREATE TABLE IF NOT EXISTS __sites_authoring_receipts(id TEXT PRIMARY KEY,request TEXT NOT NULL,response TEXT NOT NULL)").map_err(|_|Error::Storage)?;
                let request=serde_json::to_string(&input).map_err(|_|Error::Storage)?;
                let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|Error::Storage)?;
                let saved=tx.query_row("SELECT request,response FROM __sites_authoring_receipts WHERE id=?",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)));
                match saved {Ok((old,response))=>{if old!=request{return Err(Error::Conflict("IDEMPOTENCY_CONFLICT","SQL operation input changed."));}return serde_json::from_str(&response).map_err(|_|Error::Storage);},Err(rusqlite::Error::QueryReturnedNoRows)=>{},Err(_)=>return Err(Error::Storage)}
                if !database::single_statement(&input.sql) {return Err(Error::Invalid("Supply one SQL statement."));}
                let params=input.params.into_iter().map(database::parameter).collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Invalid("Unsupported SQL parameters."))?;
                tx.progress_handler(1000,Some(move ||Instant::now()>=deadline));
                tx.authorizer(Some(|ctx:AuthContext<'_>|database::authorize(ctx,true)));
                let changed=tx.execute(&input.sql,rusqlite::params_from_iter(params));
                tx.authorizer(None::<fn(AuthContext<'_>)->Authorization>);
                let changed=changed.map_err(|_|Error::Invalid("SQL failed or attempted a forbidden operation."))?;
                let invalid:i64=tx.query_row("SELECT count(*) FROM sqlite_schema WHERE lower(name) GLOB '__sites_*' AND name NOT IN ('__sites_identity','__sites_migrations','__sites_authoring_receipts')",[],|r|r.get(0)).map_err(|_|Error::Storage)?;
                if invalid!=0 {return Err(Error::Invalid("Reserved schema names cannot be used."));}
                let result=json!({"id":id,"changes":changed});
                tx.execute("INSERT INTO __sites_authoring_receipts VALUES(?,?,?)",rusqlite::params![id.to_string(),request,result.to_string()]).map_err(|_|Error::Storage)?;
                tx.commit().map_err(|_|Error::Storage)?;Ok(result)
            }).await?
        }).await?
    }
}
