//! Who is calling: passwords, JWT access tokens, refresh tokens and the `AuthUser` extractor.
//!
//! Every handler that takes an `AuthUser` parameter requires a valid
//! `Authorization: Bearer <access token>` header; the user is read from the database at
//! each request, so a disabled account or a changed role takes effect immediately.

use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

use crate::{errors::ApiError, state::SharedState};

const ISSUER: &str = "hypervisor-api";
pub const MIN_PASSWORD_LEN: usize = 8;
// Argon2 is expensive: refuse absurdly long inputs instead of hashing them.
const MAX_PASSWORD_LEN: usize = 128;
const MAX_LOGIN_FAILURES: u32 = 5;
const LOGIN_LOCK_SECONDS: i64 = 15 * 60;

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn parse(value: &str) -> Option<Role> {
        match value {
            "admin" => Some(Role::Admin),
            "user" => Some(Role::User),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }
}

/// The authenticated caller of a request.
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: String,
    pub username: String,
    pub role: Role,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    pub fn require_admin(&self) -> Result<(), ApiError> {
        if self.is_admin() {
            Ok(())
        } else {
            Err(ApiError::Forbidden("Administrator role required".into()))
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    iss: String,
    iat: i64,
    exp: i64,
}

pub fn issue_access_token(state: &SharedState, user_id: &str) -> Result<String, ApiError> {
    let iat = now();
    let claims = Claims {
        sub: user_id.to_owned(),
        iss: ISSUER.into(),
        iat,
        exp: iat + state.config.access_ttl,
    };
    jsonwebtoken::encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&state.config.jwt_secret),
    )
    .map_err(|e| {
        eprintln!("JWT signing failed: {e}");
        ApiError::Internal("Could not issue the token".into())
    })
}

fn decode_access_token(state: &SharedState, token: &str) -> Result<String, ApiError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_required_spec_claims(&["exp", "iss", "sub"]);
    jsonwebtoken::decode::<Claims>(
        token,
        &DecodingKey::from_secret(&state.config.jwt_secret),
        &validation,
    )
    .map(|data| data.claims.sub)
    .map_err(|_| ApiError::Unauthorized("Invalid or expired token".into()))
}

#[derive(FromRow)]
struct AuthRow {
    id: String,
    username: String,
    role: String,
    disabled: bool,
}

impl FromRequestParts<SharedState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &SharedState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("Missing Authorization header".into()))?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or_else(|| ApiError::Unauthorized("Expected a Bearer token".into()))?;
        let user_id = decode_access_token(state, token.trim())?;

        let row = sqlx::query_as::<_, AuthRow>(
            "SELECT id, username, role, disabled FROM users WHERE id = ?",
        )
        .bind(&user_id)
        .fetch_optional(&state.db)
        .await?
        .filter(|row| !row.disabled)
        .ok_or_else(|| ApiError::Unauthorized("Account unknown or disabled".into()))?;

        Ok(AuthUser {
            role: Role::parse(&row.role).unwrap_or(Role::User),
            id: row.id,
            username: row.username,
        })
    }
}

// ---------- passwords ----------

pub fn validate_username(username: &str) -> Result<(), ApiError> {
    let valid = (3..=32).contains(&username.chars().count())
        && username
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if valid {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Username must be 3 to 32 characters: letters, digits, '.', '_' or '-'".into(),
        ))
    }
}

pub fn validate_password(password: &str) -> Result<(), ApiError> {
    let length = password.chars().count();
    if (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&length) {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!(
            "Password must be {MIN_PASSWORD_LEN} to {MAX_PASSWORD_LEN} characters"
        )))
    }
}

pub async fn hash_password(password: String) -> Result<String, ApiError> {
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|e| {
                eprintln!("Password hashing failed: {e}");
                ApiError::Internal("Could not hash the password".into())
            })
    })
    .await
    .map_err(|e| ApiError::Internal(format!("Blocking task failed: {e}")))?
}

pub async fn verify_password(password: String, hash: String) -> bool {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash)
            .map(|parsed| {
                Argon2::default()
                    .verify_password(password.as_bytes(), &parsed)
                    .is_ok()
            })
            .unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

// ---------- random tokens ----------

/// 256 random bits, hex encoded: for refresh tokens and noVNC tokens.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

pub fn sha256_hex(value: &str) -> String {
    hex(&Sha256::digest(value.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------- brute force protection ----------

/// Refuses further attempts on an account after too many failures (per username).
pub fn check_login_allowed(state: &SharedState, username: &str) -> Result<(), ApiError> {
    let mut failures = state
        .login_failures
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let key = username.to_lowercase();
    if let Some((count, since)) = failures.get(&key).copied() {
        if now() - since >= LOGIN_LOCK_SECONDS {
            failures.remove(&key);
        } else if count >= MAX_LOGIN_FAILURES {
            return Err(ApiError::TooManyRequests(
                "Too many failed attempts, try again later".into(),
            ));
        }
    }
    Ok(())
}

pub fn record_login_failure(state: &SharedState, username: &str) {
    let mut failures = state
        .login_failures
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if failures.len() > 10_000 {
        let limit = now() - LOGIN_LOCK_SECONDS;
        failures.retain(|_, (_, since)| *since > limit);
    }
    let entry = failures
        .entry(username.to_lowercase())
        .or_insert((0, now()));
    entry.0 += 1;
}

pub fn clear_login_failures(state: &SharedState, username: &str) {
    state
        .login_failures
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&username.to_lowercase());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_restricted_to_safe_characters() {
        assert!(validate_username("alice").is_ok());
        assert!(validate_username("a.b_c-9").is_ok());
        assert!(validate_username("ab").is_err());
        assert!(validate_username("has space").is_err());
        assert!(validate_username("é-accent").is_err());
        assert!(validate_username(&"x".repeat(33)).is_err());
    }

    #[test]
    fn passwords_have_a_length_range() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("long enough").is_ok());
        assert!(validate_password(&"x".repeat(129)).is_err());
    }

    #[tokio::test]
    async fn a_password_verifies_only_against_its_own_hash() {
        let hash = hash_password("correct horse".into()).await.unwrap();
        assert!(verify_password("correct horse".into(), hash.clone()).await);
        assert!(!verify_password("wrong horse".into(), hash).await);
        assert!(!verify_password("x".into(), "not a hash".into()).await);
    }

    #[test]
    fn random_tokens_are_long_and_different() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_eq!(sha256_hex("abc").len(), 64);
    }
}
