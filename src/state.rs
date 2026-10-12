use std::sync::Arc;

use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

pub struct AppState {
    pub db: SqlitePool,
}

impl AppState {
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        let db = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS hypervisors (
                id TEXT PRIMARY KEY NOT NULL,
                name TEXT NOT NULL,
                uri TEXT NOT NULL,
                status TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;

        Ok(Self { db })
    }
}

pub type SharedState = Arc<AppState>;
