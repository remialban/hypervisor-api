use std::{collections::HashMap, sync::Arc};

use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::{config::Config, jobs::JobRegistry};

pub struct AppState {
    pub db: PgPool,
    pub jobs: JobRegistry,
    pub config: Config,
    /// Failed logins per username: (count, time of the first failure).
    pub login_failures: std::sync::Mutex<HashMap<String, (u32, i64)>>,
    /// Makes "check the quota, then create" atomic.
    pub quota_lock: tokio::sync::Mutex<()>,
    /// Serializes the rewriting of the websockify token file.
    pub vnc_file_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    /// Default (development) configuration: this is what the tests use.
    #[cfg(test)]
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        Self::with_config(database_url, Config::default()).await
    }

    pub async fn with_config(database_url: &str, config: Config) -> Result<Self, sqlx::Error> {
        let db = PgPoolOptions::new()
            .max_connections(5)
            .connect(database_url)
            .await?;

        sqlx::migrate!("./migrations").run(&db).await?;

        Ok(Self {
            db,
            jobs: JobRegistry::default(),
            config,
            login_failures: Default::default(),
            quota_lock: Default::default(),
            vnc_file_lock: Default::default(),
        })
    }
}

pub type SharedState = Arc<AppState>;
