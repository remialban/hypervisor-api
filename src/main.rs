mod access;
mod audit;
mod auth;
#[cfg(test)]
mod auth_tests;
mod common;
mod config;
mod docs;
mod errors;
mod groups;
#[cfg(test)]
mod http_tests;
mod hypervisors;
mod infrastructure;
mod jobs;
mod labs;
mod libvirt;
mod models;
mod routes;
mod shares;
mod state;
mod templates;
mod users;
mod vm_create;
mod vm_options;
mod vm_xml;
mod vms;
mod vnc;

use std::sync::Arc;

use axum::http::{HeaderValue, Method, header};
use state::AppState;
use tower_http::cors::{AllowOrigin, CorsLayer};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() {
    let config = config::Config::from_env().unwrap_or_else(|error| {
        eprintln!("Invalid configuration: {error}");
        std::process::exit(1);
    });
    let cors_origins = config.cors_origins.clone();
    let database_url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| {
            eprintln!(
                "DATABASE_URL is missing: the PostgreSQL database, for example \
                 postgres://hypervisor:password@localhost:5432/hypervisor"
            );
            std::process::exit(1);
        });

    let state = Arc::new(
        AppState::with_config(&database_url, config)
            .await
            .expect("Could not initialize the PostgreSQL database"),
    );
    if let Err(error) = users::bootstrap_admin(&state).await {
        eprintln!(
            "Could not create the first administrator: {}",
            error.message()
        );
        std::process::exit(1);
    }
    // Starts from the valid tokens only: this also drops the permanent tokens of older versions.
    if let Err(error) = vnc::sync_token_file(&state).await {
        eprintln!("Could not write the websockify token file: {error}");
    }
    vnc::spawn_cleanup(Arc::clone(&state));

    let app = routes::create_routes(state).merge(
        SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", docs::ApiDoc::openapi()),
    );
    let app = if cors_origins.is_empty() {
        app
    } else {
        app.layer(cors_layer(&cors_origins))
    };
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Could not bind port 3000");

    println!("API active sur http://0.0.0.0:3000");
    axum::serve(listener, app)
        .await
        .expect("Server execution failed");
}

/// Lets the browser front-end (served from another origin) call the API.
fn cors_layer(origins: &[String]) -> CorsLayer {
    let origins: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|origin| origin.parse().ok())
        .collect();
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(std::time::Duration::from_secs(600))
}
