//! Notifications are latency hints. Durable rows and bounded polling remain the
//! correctness path across disconnects, process restarts and duplicate delivery.
use super::{RuntimeService, error::Result};
use sqlx::postgres::{PgListener, PgPoolOptions};
use std::time::Duration;

impl RuntimeService {
    pub(super) async fn cleanup_receipts(&self) -> Result<()> {
        let mut tx = self.transaction().await?;
        sqlx::query("with expired as (select scope_kind,scope_id,key from runtime_requests where expires_at<=clock_timestamp() order by expires_at,scope_kind,scope_id,key limit 500 for update skip locked) delete from runtime_requests r using expired e where (r.scope_kind,r.scope_id,r.key)=(e.scope_kind,e.scope_id,e.key)")
            .execute(&mut *tx).await?;
        tx.commit().await?;
        // Run-scoped and worker-claim receipts have no expiry policy: retain them.
        Ok(())
    }

    /// One dedicated connection per replica, outside the request pool. Abort and
    /// await the returned task on shutdown. Polling still works if LISTEN fails.
    pub fn spawn_notification_listener(&self) -> tokio::task::JoinHandle<()> {
        let service = self.clone();
        tokio::spawn(async move {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .acquire_timeout(Duration::from_secs(5))
                .max_lifetime(None)
                .idle_timeout(None)
                .connect_lazy_with(
                    (*service.pool.connect_options())
                        .clone()
                        .application_name("platform-runtime-notifications"),
                );
            let mut backoff = Duration::from_secs(1);
            loop {
                let result: std::result::Result<(), sqlx::Error> = async {
                    let mut listener = PgListener::connect_with(&pool).await?;
                    listener.listen("platform_runtime_run").await?;
                    loop {
                        let notification = listener.recv().await?;
                        backoff = Duration::from_secs(1);
                        if let Ok(run) = notification.payload().parse() {
                            service.notify(run);
                        }
                    }
                }
                .await;
                if result.is_err() {
                    // Do not log database credentials or notification payloads.
                    tracing::warn!(
                        "Runtime notification listener disconnected; polling remains active"
                    );
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    async fn receive_run(signals: &mut tokio::sync::broadcast::Receiver<Uuid>, run: Uuid) {
        while signals.recv().await.unwrap() != run {}
    }

    async fn probe(pool: &sqlx::PgPool, signals: &mut tokio::sync::broadcast::Receiver<Uuid>) {
        let id = Uuid::now_v7();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                sqlx::query("select pg_notify('platform_runtime_run',$1)")
                    .bind(id.to_string())
                    .execute(pool)
                    .await
                    .unwrap();
                if tokio::time::timeout(Duration::from_millis(50), receive_run(signals, id))
                    .await
                    .is_ok()
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }

    #[sqlx::test(migrations = "./migrations")]
    #[ignore = "requires DATABASE_URL; isolated SQLx database"]
    async fn cross_replica_notifications_only_follow_commit(pool: sqlx::PgPool) {
        let service = RuntimeService::new(pool.clone());
        let mut signals = service.signals.subscribe();
        let listener = service.spawn_notification_listener();
        // Readiness probe: production startup does not rely on LISTEN ordering.
        probe(&pool, &mut signals).await;
        let run = Uuid::now_v7();
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("select pg_notify('platform_runtime_run',$1)")
            .bind(run.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receive_run(&mut signals, run))
                .await
                .is_err()
        );
        let mut tx = pool.begin().await.unwrap();
        sqlx::query("select pg_notify('platform_runtime_run',$1)")
            .bind(run.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), receive_run(&mut signals, run))
            .await
            .unwrap();
        // Kill only this disposable test database's notification connection.
        // LISTEN must be restored after reconnect; durable polling covers the gap.
        let disconnected: Vec<bool> = sqlx::query_scalar("select pg_terminate_backend(pid) from pg_stat_activity where datname=current_database() and application_name='platform-runtime-notifications'")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(disconnected, vec![true]);
        probe(&pool, &mut signals).await;
        listener.abort();
        let _ = listener.await;
    }
}
