//! Local browser validation with disposable PostgreSQL and SQLite data.
#[allow(dead_code)]
#[path = "../tests/sites.rs"]
mod fixture;
#[tokio::main]
async fn main() {
    let base = std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgresql:///postgres".into());
    let admin = sqlx::PgPool::connect(&base)
        .await
        .expect("Local PostgreSQL with CREATEDB is required");
    let name = format!("sites_preview_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&admin)
        .await
        .unwrap();
    let options = (*admin.connect_options()).clone().database(&name);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    fixture::browser_preview(pool.clone()).await;
    pool.close().await;
    sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)"))
        .execute(&admin)
        .await
        .unwrap();
}
