//! Login (OAuth2 token endpoint + JSON helpers), accounts, quotas and settings.

use axum::{
    Form, Json,
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    access::{self, Quota, Usage},
    audit,
    auth::{
        self, AuthUser, Role, hash_password, now, random_token, sha256_hex, validate_password,
        validate_username, verify_password,
    },
    errors::ApiError,
    models::Message,
    state::SharedState,
};

// A hash to check against when the username does not exist, so that a login attempt
// takes the same time either way (no way to discover which usernames exist).
pub const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Z1p4k2Jkq6l3yWk8X5v0t1p6mV4d3m2qgq0aYqgXn1E";

// ---------- tokens ----------

/// Answer of a successful login, as defined by OAuth2 (RFC 6749, section 5.1).
#[derive(Serialize, ToSchema)]
pub struct TokenResponse {
    /// JWT to send as `Authorization: Bearer <access_token>`.
    pub access_token: String,
    pub token_type: String,
    /// Lifetime of the access token, in seconds.
    pub expires_in: i64,
    /// Opaque token to get a new pair without the password. Single use.
    pub refresh_token: String,
}

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Body of `POST /api/auth/token` (`application/x-www-form-urlencoded`, like any OAuth2 server).
#[derive(Deserialize, ToSchema)]
pub struct TokenForm {
    /// `password` or `refresh_token`.
    pub grant_type: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub refresh_token: Option<String>,
}

async fn issue_tokens(state: &SharedState, user_id: &str) -> Result<TokenResponse, ApiError> {
    let access_token = auth::issue_access_token(state, user_id)?;
    let refresh_token = random_token();
    let created_at = now();
    sqlx::query("DELETE FROM refresh_tokens WHERE user_id = ? AND expires_at < ?")
        .bind(user_id)
        .bind(created_at)
        .execute(&state.db)
        .await?;
    sqlx::query(
        "INSERT INTO refresh_tokens (token_hash, user_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(sha256_hex(&refresh_token))
    .bind(user_id)
    .bind(created_at + state.config.refresh_ttl)
    .bind(created_at)
    .execute(&state.db)
    .await?;
    Ok(TokenResponse {
        access_token,
        token_type: "Bearer".into(),
        expires_in: state.config.access_ttl,
        refresh_token,
    })
}

#[derive(FromRow)]
struct LoginRow {
    id: String,
    username: String,
    password_hash: String,
    disabled: bool,
}

async fn login(
    state: &SharedState,
    username: &str,
    password: String,
) -> Result<TokenResponse, ApiError> {
    auth::check_login_allowed(state, username)?;
    let row = sqlx::query_as::<_, LoginRow>(
        "SELECT id, username, password_hash, disabled FROM users WHERE username = ?",
    )
    .bind(username)
    .fetch_optional(&state.db)
    .await?;

    let hash = row
        .as_ref()
        .map_or(DUMMY_HASH, |r| r.password_hash.as_str());
    let password_ok = verify_password(password, hash.to_owned()).await;
    match row {
        Some(row) if password_ok && !row.disabled => {
            auth::clear_login_failures(state, username);
            audit::insert(
                state,
                Some(&row.id),
                Some(&row.username),
                "auth.login",
                &row.id,
                "",
                true,
            )
            .await;
            issue_tokens(state, &row.id).await
        }
        _ => {
            auth::record_login_failure(state, username);
            audit::record_anonymous(state, username, "auth.login", "invalid credentials", false)
                .await;
            Err(ApiError::Unauthorized(
                "Invalid username or password".into(),
            ))
        }
    }
}

async fn refresh(state: &SharedState, refresh_token: &str) -> Result<TokenResponse, ApiError> {
    let hash = sha256_hex(refresh_token);
    // Single use: the token is consumed even if what follows fails.
    let row: Option<(String, i64)> = sqlx::query_as(
        "DELETE FROM refresh_tokens WHERE token_hash = ? RETURNING user_id, expires_at",
    )
    .bind(hash)
    .fetch_optional(&state.db)
    .await?;
    let invalid = || ApiError::Unauthorized("Invalid or expired refresh token".into());
    let (user_id, expires_at) = row.ok_or_else(invalid)?;
    if expires_at < now() {
        return Err(invalid());
    }
    let active: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM users WHERE id = ? AND disabled = 0")
            .bind(&user_id)
            .fetch_optional(&state.db)
            .await?;
    if active.is_none() {
        return Err(invalid());
    }
    issue_tokens(state, &user_id).await
}

/// A token response must never be cached by a browser or a proxy.
fn no_store(tokens: TokenResponse) -> Response {
    let mut response = Json(tokens).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

#[utoipa::path(post, path = "/api/auth/token", request_body(content = TokenForm, content_type = "application/x-www-form-urlencoded"), responses((status = 200, body = TokenResponse), (status = 400, body = Message), (status = 401, body = Message), (status = 429, body = Message)), security(()), tag = "Auth")]
pub async fn token(
    State(state): State<SharedState>,
    Form(form): Form<TokenForm>,
) -> Result<Response, ApiError> {
    let tokens = match form.grant_type.as_str() {
        "password" => {
            let username = form.username.unwrap_or_default();
            let password = form.password.unwrap_or_default();
            login(&state, &username, password).await?
        }
        "refresh_token" => {
            let token = form.refresh_token.unwrap_or_default();
            refresh(&state, &token).await?
        }
        _ => {
            return Err(ApiError::BadRequest(
                "grant_type must be 'password' or 'refresh_token'".into(),
            ));
        }
    };
    Ok(no_store(tokens))
}

#[utoipa::path(post, path = "/api/auth/login", request_body = LoginRequest, responses((status = 200, body = TokenResponse), (status = 401, body = Message), (status = 429, body = Message)), security(()), tag = "Auth")]
pub async fn login_json(
    State(state): State<SharedState>,
    Json(body): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    Ok(no_store(
        login(&state, &body.username, body.password).await?,
    ))
}

#[utoipa::path(post, path = "/api/auth/refresh", request_body = RefreshRequest, responses((status = 200, body = TokenResponse), (status = 401, body = Message)), security(()), tag = "Auth")]
pub async fn refresh_json(
    State(state): State<SharedState>,
    Json(body): Json<RefreshRequest>,
) -> Result<Response, ApiError> {
    Ok(no_store(refresh(&state, &body.refresh_token).await?))
}

#[utoipa::path(post, path = "/api/auth/logout", request_body = RefreshRequest, responses((status = 204)), security(()), tag = "Auth")]
pub async fn logout(
    State(state): State<SharedState>,
    Json(body): Json<RefreshRequest>,
) -> Result<StatusCode, ApiError> {
    let deleted: Option<String> =
        sqlx::query_scalar("DELETE FROM refresh_tokens WHERE token_hash = ? RETURNING user_id")
            .bind(sha256_hex(&body.refresh_token))
            .fetch_optional(&state.db)
            .await?;
    if let Some(user_id) = deleted {
        audit::insert(
            &state,
            Some(&user_id),
            None,
            "auth.logout",
            &user_id,
            "",
            true,
        )
        .await;
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- registration ----------

#[derive(Serialize, ToSchema)]
pub struct AuthConfig {
    /// Whether `POST /api/auth/register` is currently accepted.
    pub registration_open: bool,
}

async fn registration_open(state: &SharedState) -> Result<bool, ApiError> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'registration_open'")
            .fetch_optional(&state.db)
            .await?;
    Ok(value.as_deref() == Some("true"))
}

#[utoipa::path(get, path = "/api/auth/config", responses((status = 200, body = AuthConfig)), security(()), tag = "Auth")]
pub async fn auth_config(State(state): State<SharedState>) -> Result<Json<AuthConfig>, ApiError> {
    Ok(Json(AuthConfig {
        registration_open: registration_open(&state).await?,
    }))
}

#[utoipa::path(post, path = "/api/auth/register", request_body = LoginRequest, responses((status = 201, body = UserView), (status = 400, body = Message), (status = 403, body = Message, description = "Registration is closed"), (status = 409, body = Message)), security(()), tag = "Auth")]
pub async fn register(
    State(state): State<SharedState>,
    Json(body): Json<LoginRequest>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    if !registration_open(&state).await? {
        return Err(ApiError::Forbidden("Registration is closed".into()));
    }
    let user = insert_user(&state, &body.username, body.password, Role::User).await?;
    audit::insert(
        &state,
        Some(&user.id),
        Some(&user.username),
        "user.register",
        &user.id,
        "",
        true,
    )
    .await;
    Ok((StatusCode::CREATED, Json(user)))
}

// ---------- accounts ----------

#[derive(Serialize, ToSchema)]
pub struct UserView {
    pub id: String,
    pub username: String,
    /// `admin` or `user`.
    pub role: String,
    pub disabled: bool,
    pub created_at: i64,
    pub quota: Quota,
    pub usage: Usage,
}

#[derive(FromRow)]
struct UserRow {
    id: String,
    username: String,
    role: String,
    disabled: bool,
    created_at: i64,
}

const USER_COLUMNS: &str = "id, username, role, disabled, created_at";

async fn view(state: &SharedState, row: UserRow) -> Result<UserView, ApiError> {
    Ok(UserView {
        quota: access::quota_of(&state.db, &row.id).await?,
        usage: access::usage_of(&state.db, &row.id).await?,
        id: row.id,
        username: row.username,
        role: row.role,
        disabled: row.disabled,
        created_at: row.created_at,
    })
}

async fn load_user(state: &SharedState, id: &str) -> Result<UserView, ApiError> {
    let row =
        sqlx::query_as::<_, UserRow>(&format!("SELECT {USER_COLUMNS} FROM users WHERE id = ?"))
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| ApiError::NotFound("User not found".into()))?;
    view(state, row).await
}

/// Creates an account (used by the admin, by registration and by the first start).
pub async fn insert_user(
    state: &SharedState,
    username: &str,
    password: String,
    role: Role,
) -> Result<UserView, ApiError> {
    validate_username(username)?;
    validate_password(&password)?;
    let id = Uuid::new_v4().to_string();
    let hash = hash_password(password).await?;
    let result = sqlx::query(
        "INSERT INTO users (id, username, password_hash, role, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(username)
    .bind(hash)
    .bind(role.as_str())
    .bind(now())
    .execute(&state.db)
    .await;
    match result {
        Ok(_) => load_user(state, &id).await,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(ApiError::Conflict("This username is already taken".into()))
        }
        Err(e) => Err(e.into()),
    }
}

/// First start: creates the initial administrator when there is no account at all.
pub async fn bootstrap_admin(state: &SharedState) -> Result<(), ApiError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await?;
    if count > 0 {
        return Ok(());
    }
    let username = std::env::var("ADMIN_USERNAME")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "admin".into());
    let (password, generated) = match std::env::var("ADMIN_PASSWORD")
        .ok()
        .filter(|v| !v.is_empty())
    {
        Some(password) => (password, false),
        None => (random_token()[..24].to_owned(), true),
    };
    insert_user(state, &username, password.clone(), Role::Admin).await?;
    if generated {
        println!("First start: administrator '{username}' created with the password: {password}");
        println!("Change it now, this message is shown only once (or set ADMIN_PASSWORD).");
    } else {
        println!("First start: administrator '{username}' created.");
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/me", responses((status = 200, body = UserView), (status = 401, body = Message)), tag = "Users")]
pub async fn me(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<UserView>, ApiError> {
    Ok(Json(load_user(&state, &user.id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

#[utoipa::path(put, path = "/api/me/password", request_body = ChangePassword, responses((status = 204), (status = 400, body = Message), (status = 401, body = Message, description = "Wrong current password")), tag = "Users")]
pub async fn change_password(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<ChangePassword>,
) -> Result<StatusCode, ApiError> {
    validate_password(&body.new_password)?;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = ?")
        .bind(&user.id)
        .fetch_one(&state.db)
        .await?;
    if !verify_password(body.current_password, hash).await {
        return Err(ApiError::Unauthorized("Wrong current password".into()));
    }
    set_password(&state, &user.id, body.new_password).await?;
    audit::record(&state, &user, "user.password_change", &user.id, "").await;
    Ok(StatusCode::NO_CONTENT)
}

/// Replaces a password and closes every open session of the account.
async fn set_password(
    state: &SharedState,
    user_id: &str,
    password: String,
) -> Result<(), ApiError> {
    let hash = hash_password(password).await?;
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(hash)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM refresh_tokens WHERE user_id = ?")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(())
}

#[utoipa::path(get, path = "/api/users", responses((status = 200, body = [UserView]), (status = 403, body = Message)), tag = "Users")]
pub async fn list_users(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Vec<UserView>>, ApiError> {
    user.require_admin()?;
    let rows = sqlx::query_as::<_, UserRow>(&format!(
        "SELECT {USER_COLUMNS} FROM users ORDER BY username"
    ))
    .fetch_all(&state.db)
    .await?;
    let mut users = Vec::with_capacity(rows.len());
    for row in rows {
        users.push(view(&state, row).await?);
    }
    Ok(Json(users))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateUser {
    pub username: String,
    pub password: String,
    /// `admin` or `user` (default).
    pub role: Option<String>,
}

#[utoipa::path(post, path = "/api/users", request_body = CreateUser, responses((status = 201, body = UserView), (status = 400, body = Message), (status = 403, body = Message), (status = 409, body = Message)), tag = "Users")]
pub async fn create_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<CreateUser>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    user.require_admin()?;
    let role = parse_role(body.role.as_deref().unwrap_or("user"))?;
    let created = insert_user(&state, &body.username, body.password, role).await?;
    audit::record(&state, &user, "user.create", &created.id, &created.username).await;
    Ok((StatusCode::CREATED, Json(created)))
}

fn parse_role(value: &str) -> Result<Role, ApiError> {
    Role::parse(value).ok_or_else(|| ApiError::BadRequest("role must be 'admin' or 'user'".into()))
}

#[utoipa::path(get, path = "/api/users/{user_id}", params(("user_id" = String, Path)), responses((status = 200, body = UserView), (status = 403, body = Message), (status = 404, body = Message)), tag = "Users")]
pub async fn get_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
) -> Result<Json<UserView>, ApiError> {
    user.require_admin()?;
    Ok(Json(load_user(&state, &user_id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateUser {
    pub role: Option<String>,
    pub disabled: Option<bool>,
}

/// The system must always keep an active administrator.
async fn ensure_another_admin(state: &SharedState, user_id: &str) -> Result<(), ApiError> {
    let others: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE role = 'admin' AND disabled = 0 AND id != ?",
    )
    .bind(user_id)
    .fetch_one(&state.db)
    .await?;
    if others == 0 {
        Err(ApiError::Conflict(
            "This is the last active administrator".into(),
        ))
    } else {
        Ok(())
    }
}

#[utoipa::path(patch, path = "/api/users/{user_id}", params(("user_id" = String, Path)), request_body = UpdateUser, responses((status = 200, body = UserView), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message)), tag = "Users")]
pub async fn update_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
    Json(body): Json<UpdateUser>,
) -> Result<Json<UserView>, ApiError> {
    user.require_admin()?;
    let current = load_user(&state, &user_id).await?;
    let role = body.role.as_deref().map(parse_role).transpose()?;
    let loses_admin = current.role == "admin"
        && !current.disabled
        && (role == Some(Role::User) || body.disabled == Some(true));
    if loses_admin {
        ensure_another_admin(&state, &user_id).await?;
    }
    sqlx::query(
        "UPDATE users SET role = COALESCE(?, role), disabled = COALESCE(?, disabled) WHERE id = ?",
    )
    .bind(role.map(Role::as_str))
    .bind(body.disabled)
    .bind(&user_id)
    .execute(&state.db)
    .await?;
    if body.disabled == Some(true) {
        sqlx::query("DELETE FROM refresh_tokens WHERE user_id = ?")
            .bind(&user_id)
            .execute(&state.db)
            .await?;
    }
    audit::record(
        &state,
        &user,
        "user.update",
        &user_id,
        &format!("role={:?} disabled={:?}", body.role, body.disabled),
    )
    .await;
    Ok(Json(load_user(&state, &user_id).await?))
}

#[utoipa::path(delete, path = "/api/users/{user_id}", params(("user_id" = String, Path)), responses((status = 204), (status = 403, body = Message), (status = 404, body = Message), (status = 409, body = Message, description = "The user still owns VMs, or is the last administrator")), tag = "Users")]
pub async fn delete_user(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    let target = load_user(&state, &user_id).await?;
    if target.usage.vms > 0 {
        return Err(ApiError::Conflict(
            "This user still owns VMs: delete them or give them to someone else first".into(),
        ));
    }
    if target.role == "admin" && !target.disabled {
        ensure_another_admin(&state, &user_id).await?;
    }
    sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(&user_id)
        .execute(&state.db)
        .await?;
    audit::record(&state, &user, "user.delete", &user_id, &target.username).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ResetPassword {
    pub new_password: String,
}

#[utoipa::path(put, path = "/api/users/{user_id}/password", params(("user_id" = String, Path)), request_body = ResetPassword, responses((status = 204), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Users")]
pub async fn reset_password(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
    Json(body): Json<ResetPassword>,
) -> Result<StatusCode, ApiError> {
    user.require_admin()?;
    validate_password(&body.new_password)?;
    load_user(&state, &user_id).await?;
    set_password(&state, &user_id, body.new_password).await?;
    audit::record(&state, &user, "user.password_reset", &user_id, "").await;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/api/users/{user_id}/quota", params(("user_id" = String, Path)), request_body = Quota, responses((status = 200, body = UserView), (status = 400, body = Message), (status = 403, body = Message), (status = 404, body = Message)), tag = "Users")]
pub async fn set_quota(
    State(state): State<SharedState>,
    user: AuthUser,
    Path(user_id): Path<String>,
    Json(body): Json<Quota>,
) -> Result<Json<UserView>, ApiError> {
    user.require_admin()?;
    let limits = [
        body.max_vms,
        body.max_vcpus,
        body.max_memory_mb,
        body.max_disk_gb,
    ];
    if limits.iter().flatten().any(|limit| *limit < 0) {
        return Err(ApiError::BadRequest("Limits cannot be negative".into()));
    }
    load_user(&state, &user_id).await?;
    sqlx::query(
        "UPDATE users SET max_vms = ?, max_vcpus = ?, max_memory_mb = ?, max_disk_gb = ? WHERE id = ?",
    )
    .bind(body.max_vms)
    .bind(body.max_vcpus)
    .bind(body.max_memory_mb)
    .bind(body.max_disk_gb)
    .bind(&user_id)
    .execute(&state.db)
    .await?;
    audit::record(&state, &user, "user.quota", &user_id, &format!("{body:?}")).await;
    Ok(Json(load_user(&state, &user_id).await?))
}

// ---------- settings ----------

#[derive(Serialize, Deserialize, ToSchema)]
pub struct Settings {
    /// Lets anyone create a `user` account with `POST /api/auth/register`.
    pub registration_open: bool,
}

#[utoipa::path(get, path = "/api/settings", responses((status = 200, body = Settings), (status = 403, body = Message)), tag = "Settings")]
pub async fn get_settings(
    State(state): State<SharedState>,
    user: AuthUser,
) -> Result<Json<Settings>, ApiError> {
    user.require_admin()?;
    Ok(Json(Settings {
        registration_open: registration_open(&state).await?,
    }))
}

#[utoipa::path(put, path = "/api/settings", request_body = Settings, responses((status = 200, body = Settings), (status = 403, body = Message)), tag = "Settings")]
pub async fn put_settings(
    State(state): State<SharedState>,
    user: AuthUser,
    Json(body): Json<Settings>,
) -> Result<Json<Settings>, ApiError> {
    user.require_admin()?;
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('registration_open', ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(if body.registration_open {
        "true"
    } else {
        "false"
    })
    .execute(&state.db)
    .await?;
    audit::record(
        &state,
        &user,
        "settings.update",
        "registration_open",
        &body.registration_open.to_string(),
    )
    .await;
    Ok(Json(body))
}
