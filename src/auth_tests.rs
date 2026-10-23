//! HTTP tests of authentication, roles, ownership, sharing, quotas and the audit log.
//!
//! Same setup as `http_tests`: the real router, libvirt's in-memory test driver.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use jsonwebtoken::{EncodingKey, Header};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    auth::now,
    http_tests::{POOL, TestApp, define_vm, unique},
    state::AppState,
    users,
};

// ---------- helpers ----------

impl TestApp {
    /// Creates an account through the admin API, with a real password.
    async fn register_user(&self, username: &str, password: &str, role: &str) -> String {
        let (status, _, body) = self
            .send(
                Method::POST,
                "/api/users",
                Some(json!({ "username": username, "password": password, "role": role })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["id"].as_str().unwrap().to_string()
    }

    async fn login(&self, username: &str, password: &str) -> (StatusCode, Value) {
        let (status, _, body) = self
            .send_as(
                None,
                Method::POST,
                "/api/auth/login",
                Some(json!({ "username": username, "password": password })),
            )
            .await;
        (status, body)
    }

    async fn form(&self, path: &str, body: &str) -> (StatusCode, header::HeaderMap, Value) {
        let request = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        (
            parts.status,
            parts.headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Lets a user create VMs on the test hypervisor.
    async fn grant(&self, user_id: &str) {
        let path = format!("/api/hypervisors/{}/access/{user_id}", self.hypervisor_id);
        let (status, _) = self.status_of(Method::PUT, &path).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    async fn create_as(&self, token: &str, name: &str) -> (StatusCode, Value) {
        let body = json!({
            "name": name, "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 1 }]
        });
        let (status, _, json) = self
            .send_as(Some(token), Method::POST, &self.vms(), Some(body))
            .await;
        (status, json)
    }

    /// Creates a VM as `token` and returns its id.
    async fn owned_vm(&self, token: &str) -> String {
        let (status, vm) = self.create_as(token, &unique("owned")).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
        vm["id"].as_str().unwrap().to_string()
    }

    async fn share(&self, token: &str, vm_id: &str, username: &str) -> (StatusCode, Value) {
        let (status, _, json) = self
            .send_as(
                Some(token),
                Method::POST,
                &format!("{}/shares", self.vm(vm_id)),
                Some(json!({ "username": username })),
            )
            .await;
        (status, json)
    }

    async fn quota(&self, user_id: &str, quota: Value) -> (StatusCode, Value) {
        let (status, _, json) = self
            .send(
                Method::PUT,
                &format!("/api/users/{user_id}/quota"),
                Some(quota),
            )
            .await;
        (status, json)
    }
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap().to_string())
        .collect()
}

// ---------- authentication ----------

#[tokio::test]
async fn every_route_needs_a_token_except_the_public_ones() {
    let app = TestApp::new().await;
    for path in [
        "/api/hypervisors",
        "/api/me",
        "/api/users",
        "/api/settings",
        "/api/audit",
        "/api/jobs/x",
    ] {
        let (status, headers, body) = app.send_as(None, Method::GET, path, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["status"], "error");
        assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
    }
    let (status, _, _) = app.send_as(None, Method::GET, "/ping", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, body) = app
        .send_as(None, Method::GET, "/api/auth/config", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["registration_open"], false);
}

#[tokio::test]
async fn forged_expired_and_malformed_tokens_are_refused() {
    let app = TestApp::new().await;
    let (id, _) = app.user("someone", "user").await;
    let claims = |exp: i64| json!({ "sub": id, "iss": "hypervisor-api", "iat": 0, "exp": exp });
    let sign = |claims: Value, secret: &[u8]| {
        jsonwebtoken::encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret),
        )
        .unwrap()
    };
    let secret = app.state.config.jwt_secret.clone();

    let valid = sign(claims(now() + 60), &secret);
    let (status, _) = app.status_as(&valid, Method::GET, "/api/me").await;
    assert_eq!(status, StatusCode::OK);

    for token in [
        sign(claims(1), &secret), // expired
        sign(claims(now() + 60), b"another secret of enough length......"), // wrong key
        sign(
            json!({ "sub": id, "iss": "evil", "exp": now() + 60 }),
            &secret,
        ), // wrong issuer
        "not.a.jwt".to_string(),
    ] {
        let (status, _) = app.status_as(&token, Method::GET, "/api/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token}");
    }

    let (status, _, _) = app.send_as(None, Method::GET, "/api/me", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_user_logs_in_with_json_and_calls_the_api() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;

    let (status, tokens) = app.login("alice", "correct horse").await;

    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(tokens["expires_in"], 900);
    assert!(tokens["refresh_token"].as_str().unwrap().len() >= 32);
    let (status, me) = app
        .status_as(
            tokens["access_token"].as_str().unwrap(),
            Method::GET,
            "/api/me",
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["username"], "alice");
    assert_eq!(me["role"], "user");
    assert_eq!(me["usage"]["vms"], 0);
    assert!(me["quota"]["max_vms"].is_null(), "no quota by default");
    assert!(me.get("password_hash").is_none());
}

#[tokio::test]
async fn login_does_not_reveal_which_part_was_wrong() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;

    let (wrong_password, a) = app.login("alice", "wrong password").await;
    let (unknown_user, b) = app.login("nobody", "correct horse").await;

    assert_eq!(wrong_password, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown_user, StatusCode::UNAUTHORIZED);
    assert_eq!(a["content"], b["content"]);
    // The username is not case sensitive.
    assert_eq!(app.login("ALICE", "correct horse").await.0, StatusCode::OK);
}

#[tokio::test]
async fn too_many_failures_lock_the_account_for_a_while() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;

    for _ in 0..5 {
        assert_eq!(
            app.login("alice", "nope nope").await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    assert_eq!(
        app.login("alice", "correct horse").await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn the_oauth2_token_endpoint_speaks_forms() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;

    let (status, headers, tokens) = app
        .form(
            "/api/auth/token",
            "grant_type=password&username=alice&password=correct+horse&scope=ignored",
        )
        .await;

    assert_eq!(status, StatusCode::OK, "{tokens}");
    assert_eq!(tokens["token_type"], "Bearer");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(
        app.status_as(access, Method::GET, "/api/me").await.0,
        StatusCode::OK
    );

    let (status, _, _) = app
        .form(
            "/api/auth/token",
            "grant_type=password&username=alice&password=x",
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = app
        .form("/api/auth/token", "grant_type=client_credentials")
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_refresh_token_works_once_and_gives_a_new_pair() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;
    let (_, tokens) = app.login("alice", "correct horse").await;
    let refresh = tokens["refresh_token"].as_str().unwrap().to_string();

    let (status, _, renewed) = app
        .send_as(
            None,
            Method::POST,
            "/api/auth/refresh",
            Some(json!({ "refresh_token": refresh })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    assert_ne!(renewed["refresh_token"], tokens["refresh_token"]);
    let access = renewed["access_token"].as_str().unwrap();
    assert_eq!(
        app.status_as(access, Method::GET, "/api/me").await.0,
        StatusCode::OK
    );

    // Replaying the old one fails; so does the same grant through the OAuth2 endpoint.
    let (status, _, _) = app
        .send_as(
            None,
            Method::POST,
            "/api/auth/refresh",
            Some(json!({ "refresh_token": refresh })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = app
        .form(
            "/api/auth/token",
            &format!("grant_type=refresh_token&refresh_token={refresh}"),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The new one still works, through the OAuth2 endpoint this time.
    let new_refresh = renewed["refresh_token"].as_str().unwrap();
    let (status, _, _) = app
        .form(
            "/api/auth/token",
            &format!("grant_type=refresh_token&refresh_token={new_refresh}"),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn logout_kills_the_refresh_token() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;
    let (_, tokens) = app.login("alice", "correct horse").await;
    let body = json!({ "refresh_token": tokens["refresh_token"] });

    let (status, _, _) = app
        .send_as(None, Method::POST, "/api/auth/logout", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, _) = app
        .send_as(None, Method::POST, "/api/auth/refresh", Some(body))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_expired_refresh_token_is_refused() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;
    let (_, tokens) = app.login("alice", "correct horse").await;
    sqlx::query("UPDATE refresh_tokens SET expires_at = 1")
        .execute(&app.state.db)
        .await
        .unwrap();

    let (status, _, _) = app
        .send_as(
            None,
            Method::POST,
            "/api/auth/refresh",
            Some(json!({ "refresh_token": tokens["refresh_token"] })),
        )
        .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn disabling_an_account_cuts_its_sessions_at_once() {
    let app = TestApp::new().await;
    let id = app.register_user("alice", "correct horse", "user").await;
    let (_, tokens) = app.login("alice", "correct horse").await;
    let access = tokens["access_token"].as_str().unwrap().to_string();
    assert_eq!(
        app.status_as(&access, Method::GET, "/api/me").await.0,
        StatusCode::OK
    );

    let (status, _, _) = app
        .send(
            Method::PATCH,
            &format!("/api/users/{id}"),
            Some(json!({ "disabled": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        app.status_as(&access, Method::GET, "/api/me").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.login("alice", "correct horse").await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, _, _) = app
        .send_as(
            None,
            Method::POST,
            "/api/auth/refresh",
            Some(json!({ "refresh_token": tokens["refresh_token"] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_role_change_applies_to_tokens_already_issued() {
    let app = TestApp::new().await;
    let (id, token) = app.user("boss", "admin").await;
    assert_eq!(
        app.status_as(&token, Method::GET, "/api/users").await.0,
        StatusCode::OK
    );

    let (status, _, _) = app
        .send(
            Method::PATCH,
            &format!("/api/users/{id}"),
            Some(json!({ "role": "user" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        app.status_as(&token, Method::GET, "/api/users").await.0,
        StatusCode::FORBIDDEN
    );
}

// ---------- accounts ----------

#[tokio::test]
async fn a_user_changes_their_own_password() {
    let app = TestApp::new().await;
    app.register_user("alice", "old password", "user").await;
    let (_, tokens) = app.login("alice", "old password").await;
    let access = tokens["access_token"].as_str().unwrap();

    let (status, _, _) = app
        .send_as(
            Some(access),
            Method::PUT,
            "/api/me/password",
            Some(json!({ "current_password": "wrong", "new_password": "new password" })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = app
        .send_as(
            Some(access),
            Method::PUT,
            "/api/me/password",
            Some(json!({ "current_password": "old password", "new_password": "short" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, _) = app
        .send_as(
            Some(access),
            Method::PUT,
            "/api/me/password",
            Some(json!({ "current_password": "old password", "new_password": "new password" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert_eq!(
        app.login("alice", "old password").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(app.login("alice", "new password").await.0, StatusCode::OK);
    // The sessions opened before are closed.
    let (status, _, _) = app
        .send_as(
            None,
            Method::POST,
            "/api/auth/refresh",
            Some(json!({ "refresh_token": tokens["refresh_token"] })),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_an_admin_resets_the_password_of_someone_else() {
    let app = TestApp::new().await;
    let alice = app.register_user("alice", "old password", "user").await;
    app.register_user("bob", "bobs password", "user").await;
    let (_, bob) = app.login("bob", "bobs password").await;
    let path = format!("/api/users/{alice}/password");
    let body = json!({ "new_password": "reset password" });

    let (status, _, _) = app
        .send_as(
            bob["access_token"].as_str(),
            Method::PUT,
            &path,
            Some(body.clone()),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = app.send(Method::PUT, &path, Some(body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(app.login("alice", "reset password").await.0, StatusCode::OK);
}

#[tokio::test]
async fn registration_is_closed_until_an_admin_opens_it() {
    let app = TestApp::new().await;
    let body = json!({ "username": "newbie", "password": "some password" });

    let (status, _, _) = app
        .send_as(None, Method::POST, "/api/auth/register", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, user_token) = app.user("plain", "user").await;
    let (status, _, _) = app
        .send_as(
            Some(&user_token),
            Method::PUT,
            "/api/settings",
            Some(json!({ "registration_open": true })),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only an admin changes the flag"
    );

    let (status, _, _) = app
        .send(
            Method::PUT,
            "/api/settings",
            Some(json!({ "registration_open": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (_, _, config) = app
        .send_as(None, Method::GET, "/api/auth/config", None)
        .await;
    assert_eq!(config["registration_open"], true);

    let (status, _, created) = app
        .send_as(None, Method::POST, "/api/auth/register", Some(body.clone()))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(
        created["role"], "user",
        "registration never creates an admin"
    );
    assert_eq!(app.login("newbie", "some password").await.0, StatusCode::OK);

    app.send(
        Method::PUT,
        "/api/settings",
        Some(json!({ "registration_open": false })),
    )
    .await;
    let (status, _, _) = app
        .send_as(None, Method::POST, "/api/auth/register", Some(body))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn usernames_and_passwords_are_validated_and_unique() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;

    for (username, password, expected) in [
        ("ALICE", "another password", StatusCode::CONFLICT),
        ("has space", "another password", StatusCode::BAD_REQUEST),
        ("ab", "another password", StatusCode::BAD_REQUEST),
        ("carol", "short", StatusCode::BAD_REQUEST),
    ] {
        let (status, _, _) = app
            .send(
                Method::POST,
                "/api/users",
                Some(json!({ "username": username, "password": password })),
            )
            .await;
        assert_eq!(status, expected, "{username}");
    }
    let (status, _, _) = app
        .send(
            Method::POST,
            "/api/users",
            Some(json!({ "username": "dave", "password": "long enough", "role": "root" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_last_administrator_cannot_be_removed() {
    let app = TestApp::new().await;
    let (root_id, _) = sqlx::query_as::<_, (String, String)>("SELECT id, username FROM users")
        .fetch_one(&app.state.db)
        .await
        .unwrap();

    for request in [
        (Method::PATCH, json!({ "role": "user" })),
        (Method::PATCH, json!({ "disabled": true })),
    ] {
        let (status, _, _) = app
            .send(request.0, &format!("/api/users/{root_id}"), Some(request.1))
            .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }
    let (status, _, _) = app
        .send(Method::DELETE, &format!("/api/users/{root_id}"), None)
        .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // With a second administrator it is allowed.
    app.user("second", "admin").await;
    let (status, _, _) = app
        .send(
            Method::PATCH,
            &format!("/api/users/{root_id}"),
            Some(json!({ "role": "user" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_first_start_creates_an_administrator_only_when_there_is_none() {
    let app = TestApp::new().await;
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    users::bootstrap_admin(&app.state).await.unwrap();
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(before, after);

    let empty = Arc::new(
        AppState::new(&format!(
            "sqlite:file:{}?mode=memory&cache=shared",
            Uuid::new_v4().simple()
        ))
        .await
        .unwrap(),
    );
    users::bootstrap_admin(&empty).await.unwrap();
    let role: String = sqlx::query_scalar("SELECT role FROM users")
        .fetch_one(&empty.db)
        .await
        .unwrap();
    assert_eq!(role, "admin");
}

#[tokio::test]
async fn passwords_are_stored_hashed() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;
    let hash: String =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE username = 'alice'")
            .fetch_one(&app.state.db)
            .await
            .unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains("correct horse"));
}

#[test]
fn the_dummy_hash_used_against_unknown_users_is_a_valid_hash() {
    use argon2::password_hash::PasswordHash;
    assert!(PasswordHash::new(users::DUMMY_HASH).is_ok());
}

// ---------- roles ----------

#[tokio::test]
async fn a_basic_user_cannot_administrate() {
    let app = TestApp::new().await;
    let (id, token) = app.user("alice", "user").await;
    let hypervisor = format!("/api/hypervisors/{}", app.hypervisor_id);

    for (method, path, body) in [
        (
            Method::POST,
            "/api/hypervisors".to_string(),
            Some(json!({ "name": "x", "uri": "test:///default" })),
        ),
        (
            Method::PATCH,
            hypervisor.clone(),
            Some(json!({ "name": "x" })),
        ),
        (Method::DELETE, hypervisor.clone(), None),
        (Method::GET, format!("{hypervisor}/access"), None),
        (Method::PUT, format!("{hypervisor}/access/{id}"), None),
        (Method::GET, format!("{hypervisor}/disks"), None),
        (Method::GET, format!("{hypervisor}/pools"), None),
        (Method::GET, format!("{hypervisor}/networks"), None),
        (
            Method::POST,
            format!("{hypervisor}/networks"),
            Some(json!({ "name": "net" })),
        ),
        (Method::GET, "/api/users".to_string(), None),
        (
            Method::POST,
            "/api/users".to_string(),
            Some(json!({ "username": "zed", "password": "long enough" })),
        ),
        (Method::DELETE, format!("/api/users/{id}"), None),
        (
            Method::PUT,
            format!("/api/users/{id}/quota"),
            Some(json!({})),
        ),
        (Method::GET, "/api/settings".to_string(), None),
        (Method::GET, "/api/audit".to_string(), None),
    ] {
        let (status, _, _) = app.send_as(Some(&token), method.clone(), &path, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
}

#[tokio::test]
async fn hypervisors_are_visible_only_when_granted_and_hide_their_uri() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let path = format!("/api/hypervisors/{}", app.hypervisor_id);

    let (_, list) = app.status_as(&alice, Method::GET, "/api/hypervisors").await;
    assert_eq!(list.as_array().unwrap().len(), 0);
    assert_eq!(
        app.status_as(&alice, Method::GET, &path).await.0,
        StatusCode::NOT_FOUND
    );

    app.grant(&alice_id).await;

    let (_, list) = app.status_as(&alice, Method::GET, "/api/hypervisors").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["uri"], "", "the URI is for admins only");
    let (status, one) = app.status_as(&alice, Method::GET, &path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["uri"], "");
    let (_, admin_view) = app.status_of(Method::GET, &path).await;
    assert_eq!(admin_view["uri"], "test:///default");

    // The admin sees who has access, and can take it back.
    let (_, entries) = app.status_of(Method::GET, &format!("{path}/access")).await;
    assert_eq!(entries[0]["username"], "alice");
    assert_eq!(
        app.status_of(Method::DELETE, &format!("{path}/access/{alice_id}"))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.status_of(Method::DELETE, &format!("{path}/access/{alice_id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.status_as(&alice, Method::GET, &path).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn creating_a_vm_needs_a_grant_on_the_hypervisor() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;

    let (status, _) = app.create_as(&alice, &unique("denied")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let options = format!("/api/hypervisors/{}/vm-options", app.hypervisor_id);
    assert_eq!(
        app.status_as(&alice, Method::GET, &options).await.0,
        StatusCode::FORBIDDEN
    );

    app.grant(&alice_id).await;

    assert_eq!(
        app.status_as(&alice, Method::GET, &options).await.0,
        StatusCode::OK
    );
    let (status, vm) = app.create_as(&alice, &unique("mine")).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["owner"], "alice");
    assert_eq!(vm["access"], "owner");
}

#[tokio::test]
async fn a_user_sees_only_their_own_vms() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (bob_id, bob) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    app.grant(&bob_id).await;
    let alices = unique("alices");
    let bobs = unique("bobs");
    let alice_vm = app.create_as(&alice, &alices).await.1["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.create_as(&bob, &bobs).await;
    let unowned = unique("unowned");
    let unowned_id = define_vm(&unowned, &[]);

    let (_, list) = app.status_as(&alice, Method::GET, &app.vms()).await;
    assert_eq!(names(&list), vec![alices.clone()]);

    // Someone else's VM, or a VM nobody owns, looks like it does not exist.
    for path in [app.vm(&unowned_id), app.vm(&alice_vm)] {
        assert_eq!(
            app.status_as(&bob, Method::GET, &path).await.0,
            StatusCode::NOT_FOUND
        );
    }
    for action in ["start", "stop"] {
        let path = format!("{}/{action}", app.vm(&alice_vm));
        assert_eq!(
            app.status_as(&bob, Method::POST, &path).await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        app.status_as(&bob, Method::DELETE, &app.vm(&alice_vm))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vm("not-a-uuid"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );

    // The admin sees everything, with the owner of each VM.
    let (_, all) = app.status_of(Method::GET, &app.vms()).await;
    let all = all.as_array().unwrap();
    let find = |name: &str| all.iter().find(|vm| vm["name"] == name).unwrap().clone();
    assert_eq!(find(&alices)["owner"], "alice");
    assert_eq!(find(&bobs)["owner"], "bob");
    assert!(find(&unowned)["owner"].is_null());
    assert_eq!(find(&unowned)["access"], "admin");
}

#[tokio::test]
async fn an_admin_can_create_a_vm_for_a_user_but_a_user_cannot() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (bob_id, _) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let body = |owner: &str| {
        json!({
            "name": unique("for"), "memory_mb": 128, "vcpus": 1,
            "disks": [{ "pool": POOL, "size_gb": 1 }], "owner_id": owner
        })
    };

    let (status, _, _) = app
        .send_as(Some(&alice), Method::POST, &app.vms(), Some(body(&bob_id)))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, vm) = {
        let (s, _, j) = app
            .send(Method::POST, &app.vms(), Some(body(&alice_id)))
            .await;
        (s, j)
    };
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    assert_eq!(vm["owner"], "alice");
    let (_, list) = app.status_as(&alice, Method::GET, &app.vms()).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, _, _) = app
        .send(Method::POST, &app.vms(), Some(body("no-such-user")))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_user_keeps_their_vms_when_the_grant_is_removed() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;

    let path = format!("/api/hypervisors/{}/access/{alice_id}", app.hypervisor_id);
    app.status_of(Method::DELETE, &path).await;

    assert_eq!(
        app.status_as(&alice, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        app.status_as(&alice, Method::POST, &format!("{}/start", app.vm(&vm)))
            .await
            .0,
        StatusCode::OK
    );
    let (status, _) = app.create_as(&alice, &unique("again")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The hypervisor still shows up, since it hosts one of their VMs.
    let (_, list) = app.status_as(&alice, Method::GET, "/api/hypervisors").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_owner_can_start_stop_clone_and_delete() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;

    let start = app
        .status_as(&alice, Method::POST, &format!("{}/start", app.vm(&vm)))
        .await;
    assert_eq!(start.0, StatusCode::OK, "{}", start.1);
    let stop = app
        .status_as(
            &alice,
            Method::POST,
            &format!("{}/stop?force=true", app.vm(&vm)),
        )
        .await;
    assert_eq!(stop.0, StatusCode::OK, "{}", stop.1);

    let clone_name = unique("clone");
    let (status, _, job) = app
        .send_as(
            Some(&alice),
            Method::POST,
            &format!("{}/clone", app.vm(&vm)),
            Some(json!({ "name": clone_name })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{job}");
    let job_id = job["id"].as_str().unwrap();
    assert!(
        job.get("owner_id").is_none(),
        "internal fields are not exposed"
    );
    let finished = loop {
        let (status, job) = app
            .status_as(&alice, Method::GET, &format!("/api/jobs/{job_id}"))
            .await;
        assert_eq!(status, StatusCode::OK);
        if job["status"] != "running" {
            break job;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_eq!(finished["status"], "succeeded", "{finished}");
    let clone_id = finished["result"]["id"].as_str().unwrap();
    assert_eq!(
        app.status_as(&alice, Method::GET, &app.vm(clone_id))
            .await
            .1["owner"],
        "alice"
    );

    for id in [clone_id, vm.as_str()] {
        let url = format!("{}?delete_disks=true", app.vm(id));
        assert_eq!(
            app.status_as(&alice, Method::DELETE, &url).await.0,
            StatusCode::NO_CONTENT
        );
    }
    let owners: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vm_owners")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(owners, 0, "ownership is forgotten with the VM");
}

#[tokio::test]
async fn a_job_is_private_to_its_owner_and_the_admins() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (_, bob) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let (_, _, job) = app
        .send_as(
            Some(&alice),
            Method::POST,
            &format!("{}/clone", app.vm(&vm)),
            Some(json!({ "name": unique("c") })),
        )
        .await;
    let path = format!("/api/jobs/{}", job["id"].as_str().unwrap());

    assert_eq!(
        app.status_as(&bob, Method::GET, &path).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.status_as(&alice, Method::GET, &path).await.0,
        StatusCode::OK
    );
    assert_eq!(app.status_of(Method::GET, &path).await.0, StatusCode::OK);
}

#[tokio::test]
async fn only_an_admin_gives_a_vm_to_someone_else() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (bob_id, bob) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let path = format!("{}/owner", app.vm(&vm));

    let (status, _, _) = app
        .send_as(
            Some(&alice),
            Method::PUT,
            &path,
            Some(json!({ "owner_id": bob_id })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, moved) = app
        .send(Method::PUT, &path, Some(json!({ "owner_id": bob_id })))
        .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["owner"], "bob");
    assert_eq!(
        app.status_as(&alice, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::OK
    );

    // This is also how a VM created outside the API gets an owner.
    let outside = define_vm(&unique("outside"), &[]);
    let (status, _, adopted) = app
        .send(
            Method::PUT,
            &format!("{}/owner", app.vm(&outside)),
            Some(json!({ "owner_id": alice_id })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{adopted}");
    assert_eq!(
        app.status_as(&alice, Method::GET, &app.vm(&outside))
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_user_who_owns_vms_cannot_be_deleted() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let path = format!("/api/users/{alice_id}");

    assert_eq!(
        app.status_of(Method::DELETE, &path).await.0,
        StatusCode::CONFLICT
    );

    app.status_as(&alice, Method::DELETE, &app.vm(&vm)).await;
    assert_eq!(
        app.status_of(Method::DELETE, &path).await.0,
        StatusCode::NO_CONTENT
    );
}

// ---------- sharing ----------

#[tokio::test]
async fn a_borrower_gets_the_console_and_power_but_nothing_else() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (_, bob) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let url = app.vm(&vm);

    assert_eq!(
        app.status_as(&bob, Method::GET, &url).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, share) = app.share(&alice, &vm, "bob").await;
    assert_eq!(status, StatusCode::CREATED, "{share}");

    // Sees it, with the right label.
    let (status, seen) = app.status_as(&bob, Method::GET, &url).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seen["access"], "shared");
    assert_eq!(seen["owner"], "alice");
    let (_, list) = app.status_as(&bob, Method::GET, &app.vms()).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    // Even though Bob has no access to the hypervisor itself.
    let hypervisor = format!("/api/hypervisors/{}", app.hypervisor_id);
    assert_eq!(
        app.status_as(&bob, Method::GET, &hypervisor).await.0,
        StatusCode::OK
    );
    let (_, hypervisors) = app.status_as(&bob, Method::GET, "/api/hypervisors").await;
    assert_eq!(hypervisors.as_array().unwrap().len(), 1);
    assert_eq!(
        app.status_as(&bob, Method::GET, &format!("{hypervisor}/vm-options"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, _) = app.create_as(&bob, &unique("nope")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "no VM creation on that hypervisor"
    );

    // Power: allowed.
    assert_eq!(
        app.status_as(&bob, Method::POST, &format!("{url}/start"))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app.status_as(&bob, Method::POST, &format!("{url}/stop?force=true"))
            .await
            .0,
        StatusCode::OK
    );
    // Everything else: forbidden.
    assert_eq!(
        app.status_as(&bob, Method::DELETE, &url).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, _, _) = app
        .send_as(
            Some(&bob),
            Method::POST,
            &format!("{url}/clone"),
            Some(json!({ "name": unique("c") })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.status_as(&bob, Method::GET, &format!("{url}/shares"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.share(&bob, &vm, "alice").await.0,
        StatusCode::FORBIDDEN,
        "no lending further"
    );
    let (status, _, _) = app
        .send_as(
            Some(&bob),
            Method::PUT,
            &format!("{url}/owner"),
            Some(json!({ "owner_id": alice_id })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The VM still exists.
    assert_eq!(
        app.status_as(&alice, Method::GET, &url).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_owner_lists_and_revokes_shares_and_a_borrower_can_leave() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (bob_id, bob) = app.user("bob", "user").await;
    let (carol_id, carol) = app.user("carol", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let shares = format!("{}/shares", app.vm(&vm));
    app.share(&alice, &vm, "bob").await;
    app.share(&alice, &vm, "carol").await;

    let (_, listed) = app.status_as(&alice, Method::GET, &shares).await;
    assert_eq!(names_of(&listed), vec!["bob", "carol"]);

    // Bob cannot remove Carol, but can give the VM back himself.
    assert_eq!(
        app.status_as(&bob, Method::DELETE, &format!("{shares}/{carol_id}"))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.status_as(&bob, Method::DELETE, &format!("{shares}/{bob_id}"))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::NOT_FOUND
    );

    // The owner revokes Carol; the admin can see and do the same.
    assert_eq!(
        app.status_as(&alice, Method::DELETE, &format!("{shares}/{carol_id}"))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.status_as(&carol, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.status_as(&alice, Method::DELETE, &format!("{shares}/{carol_id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.status_of(Method::GET, &shares).await.0, StatusCode::OK);
}

fn names_of(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|v| v["username"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn sharing_has_sanity_checks() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;

    assert_eq!(
        app.share(&alice, &vm, "ghost").await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.share(&alice, &vm, "alice").await.0,
        StatusCode::BAD_REQUEST,
        "to oneself"
    );
    let (status, _, _) = app
        .send_as(
            Some(&alice),
            Method::POST,
            &format!("{}/shares", app.vm(&vm)),
            Some(json!({ "username": "bob", "expires_at": 5 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "an end date in the past");

    // A VM nobody owns cannot be lent (the admin gives it to someone first).
    let outside = define_vm(&unique("outside"), &[]);
    assert_eq!(
        app.share(&app.admin_token, &outside, "bob").await.0,
        StatusCode::CONFLICT
    );
    // Lending twice just updates the loan.
    assert_eq!(app.share(&alice, &vm, "bob").await.0, StatusCode::CREATED);
    assert_eq!(app.share(&alice, &vm, "bob").await.0, StatusCode::CREATED);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vm_shares")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn a_loan_ends_on_its_own_and_with_the_vm() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (_, bob) = app.user("bob", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    let (status, _, _) = app
        .send_as(
            Some(&alice),
            Method::POST,
            &format!("{}/shares", app.vm(&vm)),
            Some(json!({ "username": "bob", "expires_at": now() + 3600 })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::OK
    );

    sqlx::query("UPDATE vm_shares SET expires_at = 1")
        .execute(&app.state.db)
        .await
        .unwrap();

    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vm(&vm)).await.0,
        StatusCode::NOT_FOUND
    );
    // Bob no longer has anything on this hypervisor: it is not even visible to him.
    assert_eq!(
        app.status_as(&bob, Method::GET, &app.vms()).await.0,
        StatusCode::NOT_FOUND
    );
    let (status, listed) = app
        .status_as(&alice, Method::GET, &format!("{}/shares", app.vm(&vm)))
        .await;
    assert_eq!(
        (status, listed.as_array().map(Vec::len)),
        (StatusCode::OK, Some(0)),
        "{listed}"
    );

    // Deleting the VM deletes its loans.
    app.share(&alice, &vm, "bob").await;
    app.status_as(&alice, Method::DELETE, &app.vm(&vm)).await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vm_shares")
        .fetch_one(&app.state.db)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn the_console_follows_the_same_rules() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    let (_, bob) = app.user("bob", "user").await;
    let (_, carol) = app.user("carol", "user").await;
    app.grant(&alice_id).await;
    let vm = app.owned_vm(&alice).await;
    app.share(&alice, &vm, "bob").await;
    let path = format!("{}/vnc", app.vm(&vm));

    // Someone with no right gets a 404 before libvirt is even asked.
    assert_eq!(
        app.status_as(&carol, Method::GET, &path).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.send_as(None, Method::GET, &path, None).await.0,
        StatusCode::UNAUTHORIZED
    );

    // The owner and the borrower get past the access check: the VM is stopped, so there is
    // no VNC port yet (409), which is the same answer for both.
    for token in [&alice, &bob, &app.admin_token] {
        let (status, body) = app.status_as(token, Method::GET, &path).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
    }
}

#[tokio::test]
async fn console_tokens_are_random_short_lived_and_written_for_websockify() {
    let app = TestApp::new().await;
    let a = crate::auth::random_token();
    let b = crate::auth::random_token();
    sqlx::query("INSERT INTO vnc_tokens (token, target, expires_at) VALUES (?, 'host:5900', ?), (?, 'host:5901', 1)")
        .bind(&a).bind(now() + 60).bind(&b)
        .execute(&app.state.db).await.unwrap();

    crate::vnc::sync_token_file(&app.state).await.unwrap();

    let file = std::fs::read_to_string(&app.state.config.vnc_token_file).unwrap();
    assert_eq!(
        file,
        format!("{a}: host:5900\n"),
        "the expired token is gone from the file"
    );
}

// ---------- quotas ----------

#[tokio::test]
async fn there_is_no_quota_by_default_and_the_admin_can_set_one() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;
    app.owned_vm(&alice).await;
    app.owned_vm(&alice).await;
    app.owned_vm(&alice).await;

    let (_, me) = app.status_as(&alice, Method::GET, "/api/me").await;
    assert_eq!(
        me["usage"],
        json!({ "vms": 3, "vcpus": 3, "memory_mb": 384, "disk_gb": 3 })
    );

    let (status, user) = app.quota(&alice_id, json!({ "max_vms": 3 })).await;
    assert_eq!(status, StatusCode::OK, "{user}");
    assert_eq!(user["quota"]["max_vms"], 3);
    assert!(user["quota"]["max_vcpus"].is_null());
    let (status, refusal) = app.create_as(&alice, &unique("over")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(refusal["content"].as_str().unwrap().contains("max_vms"));

    // A user cannot raise their own quota; the admin lifts it.
    let (status, _, _) = app
        .send_as(
            Some(&alice),
            Method::PUT,
            &format!("/api/users/{alice_id}/quota"),
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    app.quota(&alice_id, json!({})).await;
    assert_eq!(
        app.create_as(&alice, &unique("free")).await.0,
        StatusCode::CREATED
    );
    assert_eq!(
        app.quota(&alice_id, json!({ "max_vms": -1 })).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn every_resource_is_limited() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;

    for (limit, field) in [
        (json!({ "max_vcpus": 0 }), "max_vcpus"),
        (json!({ "max_memory_mb": 100 }), "max_memory_mb"),
        (json!({ "max_disk_gb": 0 }), "max_disk_gb"),
    ] {
        app.quota(&alice_id, limit).await;
        let (status, refusal) = app.create_as(&alice, &unique("big")).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{field}");
        assert!(refusal["content"].as_str().unwrap().contains(field));
    }
}

#[tokio::test]
async fn deleting_a_vm_frees_its_quota_and_a_clone_counts() {
    let app = TestApp::new().await;
    let (alice_id, alice) = app.user("alice", "user").await;
    app.grant(&alice_id).await;
    app.quota(&alice_id, json!({ "max_vms": 1 })).await;
    let vm = app.owned_vm(&alice).await;

    // Cloning would make a second VM.
    let (status, _, refusal) = app
        .send_as(
            Some(&alice),
            Method::POST,
            &format!("{}/clone", app.vm(&vm)),
            Some(json!({ "name": unique("c") })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");

    app.status_as(&alice, Method::DELETE, &app.vm(&vm)).await;
    assert_eq!(
        app.create_as(&alice, &unique("again")).await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn quotas_are_counted_for_the_owner_when_an_admin_creates_the_vm() {
    let app = TestApp::new().await;
    let (alice_id, _) = app.user("alice", "user").await;
    app.quota(&alice_id, json!({ "max_vms": 0 })).await;

    let (status, _, refusal) = app
        .send(
            Method::POST,
            &app.vms(),
            Some(json!({
                "name": unique("for"), "memory_mb": 128, "vcpus": 1,
                "disks": [{ "pool": POOL, "size_gb": 1 }], "owner_id": alice_id
            })),
        )
        .await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{refusal}");
}

// ---------- audit ----------

#[tokio::test]
async fn the_audit_log_tells_who_did_what() {
    let app = TestApp::new().await;
    app.register_user("alice", "correct horse", "user").await;
    app.login("alice", "wrong password").await;
    let (_, tokens) = app.login("alice", "correct horse").await;
    let alice = tokens["access_token"].as_str().unwrap();
    let (alice_id, _) = sqlx::query_as::<_, (String, String)>(
        "SELECT id, username FROM users WHERE username = 'alice'",
    )
    .fetch_one(&app.state.db)
    .await
    .unwrap();
    app.grant(&alice_id).await;
    let vm = app.owned_vm(alice).await;
    app.status_as(alice, Method::POST, &format!("{}/start", app.vm(&vm)))
        .await;

    assert_eq!(
        app.status_as(alice, Method::GET, "/api/audit").await.0,
        StatusCode::FORBIDDEN
    );
    let (status, log) = app.status_of(Method::GET, "/api/audit?limit=500").await;
    assert_eq!(status, StatusCode::OK);
    let entries = log.as_array().unwrap();
    let has = |action: &str, username: &str, success: bool| {
        entries
            .iter()
            .any(|e| e["action"] == action && e["username"] == username && e["success"] == success)
    };
    assert!(has("user.create", "root-test", true));
    assert!(
        has("auth.login", "alice", false),
        "failed logins are recorded"
    );
    assert!(has("auth.login", "alice", true));
    assert!(has("hypervisor.grant", "root-test", true));
    assert!(has("vm.create", "alice", true));
    assert!(has("vm.start", "alice", true));
    assert!(
        !log.to_string().contains("correct horse"),
        "no secret in the log"
    );
    assert_eq!(entries[0]["action"], "vm.start", "newest first");

    let (_, only_creations) = app
        .status_of(Method::GET, "/api/audit?action=vm.create")
        .await;
    assert_eq!(only_creations.as_array().unwrap().len(), 1);
    let (_, by_user) = app
        .status_of(
            Method::GET,
            &format!("/api/audit?user_id={alice_id}&limit=1"),
        )
        .await;
    assert_eq!(by_user.as_array().unwrap().len(), 1);
}
