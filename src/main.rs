mod common;
mod docs;
mod errors;
mod hypervisors;
mod infrastructure;
mod libvirt;
mod models;
mod routes;
mod state;
mod vms;
mod vnc;

use std::sync::Arc;

use state::AppState;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() {
    let state = Arc::new(
        AppState::new("sqlite://api.db?mode=rwc")
            .await
            .expect("Could not initialize the SQLite database"),
    );
    let app = routes::create_routes(state).merge(
        SwaggerUi::new("/swagger-ui")
            .url("/api-docs/openapi.json", docs::ApiDoc::openapi()),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Could not bind port 3000");

    println!("API active sur http://0.0.0.0:3000");
    axum::serve(listener, app)
        .await
        .expect("Server execution failed");

        
}
