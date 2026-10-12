mod common;
mod docs;
mod hypervisors;
mod infrastructure;
mod models;
mod routes;
mod state;
mod vms;

use std::sync::Arc;

use state::AppState;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[tokio::main]
async fn main() {
    let state = Arc::new(
        AppState::new("sqlite://api.db?mode=rwc")
            .await
            .expect("Impossible d'initialiser la base SQLite"),
    );
    let app = routes::creer_routes(state).merge(
        SwaggerUi::new("/swagger-ui")
            .url("/api-docs/openapi.json", docs::ApiDoc::openapi()),
    );
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("Impossible d'ouvrir le port 3000");

    println!("API active sur http://0.0.0.0:3000");
    axum::serve(listener, app)
        .await
        .expect("Erreur pendant l'execution du serveur");

        
}
