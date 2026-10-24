//! Settings read once at startup (environment variables), kept in `AppState`.

use std::path::PathBuf;

use rand_core::{OsRng, RngCore};

const MIN_SECRET_LEN: usize = 32;

pub struct Config {
    /// Signs the access tokens (JWT, HS256).
    pub jwt_secret: Vec<u8>,
    /// Lifetime of an access token, in seconds.
    pub access_ttl: i64,
    /// Lifetime of a refresh token, in seconds.
    pub refresh_ttl: i64,
    /// Lifetime of a noVNC token, in seconds.
    pub vnc_ttl: i64,
    /// File read by websockify (`token: host:port` lines).
    pub vnc_token_file: PathBuf,
    /// Origins allowed to call the API from a browser (CORS). Empty = same origin only.
    pub cors_origins: Vec<String>,
}

impl Default for Config {
    /// Development defaults. The random secret changes at every start, which is safe
    /// (tokens simply stop working after a restart).
    fn default() -> Self {
        let mut secret = vec![0u8; 48];
        OsRng.fill_bytes(&mut secret);
        Self {
            jwt_secret: secret,
            access_ttl: 15 * 60,
            refresh_ttl: 7 * 24 * 3600,
            vnc_ttl: 120,
            vnc_token_file: PathBuf::from("vm.cfg"),
            cors_origins: Vec::new(),
        }
    }
}

impl Config {
    /// `JWT_SECRET` (or `JWT_SECRET_FILE`, for Docker secrets), `ACCESS_TOKEN_TTL_SECONDS`,
    /// `REFRESH_TOKEN_TTL_SECONDS`, `VNC_TOKEN_TTL_SECONDS`, `VM_CONFIG_PATH`,
    /// `CORS_ALLOWED_ORIGINS` (comma separated).
    pub fn from_env() -> Result<Self, String> {
        let mut config = Self::default();

        let secret = match (env("JWT_SECRET"), env("JWT_SECRET_FILE")) {
            (Some(secret), _) => Some(secret),
            (None, Some(path)) => Some(
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("Cannot read JWT_SECRET_FILE {path}: {e}"))?
                    .trim()
                    .to_owned(),
            ),
            (None, None) => None,
        };
        match secret {
            Some(secret) if secret.len() >= MIN_SECRET_LEN => {
                config.jwt_secret = secret.into_bytes()
            }
            Some(_) => {
                return Err(format!(
                    "JWT_SECRET must be at least {MIN_SECRET_LEN} characters"
                ));
            }
            None => eprintln!(
                "JWT_SECRET is not set: using a random secret, sessions will not survive a restart"
            ),
        }

        config.access_ttl = seconds("ACCESS_TOKEN_TTL_SECONDS", config.access_ttl)?;
        config.refresh_ttl = seconds("REFRESH_TOKEN_TTL_SECONDS", config.refresh_ttl)?;
        config.vnc_ttl = seconds("VNC_TOKEN_TTL_SECONDS", config.vnc_ttl)?;
        if let Some(path) = env("VM_CONFIG_PATH") {
            config.vnc_token_file = PathBuf::from(path);
        }
        if let Some(origins) = env("CORS_ALLOWED_ORIGINS") {
            config.cors_origins = origins
                .split(',')
                .map(|o| o.trim().trim_end_matches('/').to_owned())
                .filter(|o| !o.is_empty())
                .collect();
        }
        Ok(config)
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn seconds(name: &str, default: i64) -> Result<i64, String> {
    match env(name) {
        None => Ok(default),
        Some(value) => value
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|v| *v > 0)
            .ok_or_else(|| format!("{name} must be a positive number of seconds")),
    }
}
