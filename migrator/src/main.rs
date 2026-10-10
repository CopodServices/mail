use migration::{Migrator, MigratorTrait};
use tracing::{error, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("❌ Missing environment variable: DATABASE_URL");
            std::process::exit(1);
        }
    };

    match sea_orm::Database::connect(&database_url).await {
        Ok(conn) => {
            info!("✅ Connected to database");
            Migrator::up(&conn, None).await.unwrap();
            info!("✅ Migration ran");
        }
        Err(err) => {
            error!("❌ Failed to connect to database: {}", err);
            std::process::exit(1);
        }
    };
}
