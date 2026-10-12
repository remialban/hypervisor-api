use axum::{http::StatusCode, Json};

use crate::models::Message;

#[utoipa::path(
    get,
    path = "/ping",
    responses((status = 200, body = Message)),
    tag = "Infrastructure"
)]
pub async fn ping() -> Json<Message> {
    Json(Message {
        statut: "ok".into(),
        contenu: "pong".into(),
    })
}

pub fn non_implemente(message: &str) -> (StatusCode, Json<Message>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(Message {
            statut: "non_implemente".into(),
            contenu: message.into(),
        }),
    )
}

pub fn erreur(status: StatusCode, message: &str) -> (StatusCode, Json<Message>) {
    (
        status,
        Json(Message {
            statut: "erreur".into(),
            contenu: message.into(),
        }),
    )
}
