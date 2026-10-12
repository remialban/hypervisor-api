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
        status: "ok".into(),
        content: "pong".into(),
    })
}

pub fn not_implemented(message: &str) -> (StatusCode, Json<Message>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(Message {
            status: "not_implemented".into(),
            content: message.into(),
        }),
    )
}

pub fn error_response(status: StatusCode, message: &str) -> (StatusCode, Json<Message>) {
    (
        status,
        Json(Message {
            status: "error".into(),
            content: message.into(),
        }),
    )
}
