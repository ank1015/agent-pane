//! Apply Platform migrations without starting HTTP listeners or gateway clients.
use platform_server::projects::ProjectDatabase;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    let url = zeroize::Zeroizing::new(std::env::var("PLATFORM_SERVER_DATABASE_URL")?);
    let database = ProjectDatabase::connect(&url, 0, 1, Duration::from_secs(10)).await?;
    database.migrate().await?;
    println!("Platform database migrations applied successfully.");
    database.pool().close().await;
    Ok(())
}
