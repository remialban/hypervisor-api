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
                nom TEXT NOT NULL,
                uri TEXT NOT NULL,
                statut TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS vms (
                id TEXT PRIMARY KEY NOT NULL,
                hypervisor_id TEXT NOT NULL,
                nom TEXT NOT NULL,
                memoire_mb INTEGER NOT NULL,
                vcpus INTEGER NOT NULL,
                statut TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS disks (
                id TEXT PRIMARY KEY NOT NULL,
                hypervisor_id TEXT NOT NULL,
                nom TEXT NOT NULL,
                taille_gb INTEGER NOT NULL,
                format TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;

        sqlx::query(
            "CREATE TABLE IF NOT EXISTS networks (
                id TEXT PRIMARY KEY NOT NULL,
                hypervisor_id TEXT NOT NULL,
                nom TEXT NOT NULL,
                bridge TEXT,
                statut TEXT NOT NULL
            )",
        )
        .execute(&db)
        .await?;

        Ok(Self { db })
    }
}

pub type SharedState = Arc<AppState>;
