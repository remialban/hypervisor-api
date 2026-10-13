use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use virt::error::ErrorNumber;

use crate::models::Message;

// Every way a request can fail. Handlers return `Result<T, ApiError>`.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Conflict(String),
    BadRequest(String),
    BadGateway(String),
    GatewayTimeout(String),
    Internal(String),
}

// Axum calls this to turn an error into an HTTP response (like @ExceptionHandler).
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, content) = match self {
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::BadGateway(m) => (StatusCode::BAD_GATEWAY, m),
            ApiError::GatewayTimeout(m) => (StatusCode::GATEWAY_TIMEOUT, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        let body = Message {
            status: "error".into(),
            content,
        };
        (status, Json(body)).into_response()
    }
}

// Lets `?` convert a libvirt error into an ApiError automatically.
impl From<virt::error::Error> for ApiError {
    fn from(err: virt::error::Error) -> Self {
        match err.code() {
            ErrorNumber::NoDomain => ApiError::NotFound("VM not found".into()),
            ErrorNumber::OperationInvalid => {
                ApiError::Conflict("Operation not allowed in the current VM state".into())
            }
            ErrorNumber::DomExist => ApiError::Conflict("A VM with this name or UUID already exists".into()),
            ErrorNumber::InvalidArg => ApiError::BadRequest("Invalid argument".into()),
            ErrorNumber::OperationTimeout => ApiError::GatewayTimeout("The hypervisor timed out".into()),
            ErrorNumber::NoConnect | ErrorNumber::SystemError => {
                ApiError::BadGateway("Hypervisor unreachable".into())
            }
            // Raw libvirt messages may leak paths or hosts: log them, don't return them.
            _ => {
                eprintln!("libvirt error: {err}");
                ApiError::Internal("Unexpected hypervisor error".into())
            }
        }
    }
}
