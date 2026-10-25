use axum::{
    Json,
    http::{HeaderValue, StatusCode, header::WWW_AUTHENTICATE},
    response::{IntoResponse, Response},
};
use virt::error::ErrorNumber;

use crate::models::Message;

// Every way a request can fail. Handlers return `Result<T, ApiError>`.
#[derive(Debug)]
pub enum ApiError {
    /// Missing, invalid or expired credentials.
    Unauthorized(String),
    /// Authenticated, but not allowed to do this.
    Forbidden(String),
    TooManyRequests(String),
    NotFound(String),
    Conflict(String),
    BadRequest(String),
    BadGateway(String),
    GatewayTimeout(String),
    Internal(String),
}

impl ApiError {
    pub fn message(&self) -> &str {
        match self {
            ApiError::Unauthorized(m)
            | ApiError::Forbidden(m)
            | ApiError::TooManyRequests(m)
            | ApiError::NotFound(m)
            | ApiError::Conflict(m)
            | ApiError::BadRequest(m)
            | ApiError::BadGateway(m)
            | ApiError::GatewayTimeout(m)
            | ApiError::Internal(m) => m,
        }
    }

    fn into_parts(self) -> (StatusCode, String) {
        match self {
            ApiError::Unauthorized(m) => (StatusCode::UNAUTHORIZED, m),
            ApiError::Forbidden(m) => (StatusCode::FORBIDDEN, m),
            ApiError::TooManyRequests(m) => (StatusCode::TOO_MANY_REQUESTS, m),
            ApiError::NotFound(m) => (StatusCode::NOT_FOUND, m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, m),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            ApiError::BadGateway(m) => (StatusCode::BAD_GATEWAY, m),
            ApiError::GatewayTimeout(m) => (StatusCode::GATEWAY_TIMEOUT, m),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        }
    }
}

// Axum calls this to turn an error into an HTTP response (like @ExceptionHandler).
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let unauthorized = matches!(self, ApiError::Unauthorized(_));
        let (status, body) = <(StatusCode, Json<Message>)>::from(self);
        let mut response = (status, body).into_response();
        if unauthorized {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

// Older handlers return `(StatusCode, Json<Message>)`: `?` converts an ApiError for them.
impl From<ApiError> for (StatusCode, Json<Message>) {
    fn from(err: ApiError) -> Self {
        let (status, content) = err.into_parts();
        (
            status,
            Json(Message {
                status: "error".into(),
                content,
            }),
        )
    }
}

// Lets `?` convert a database error into an ApiError automatically.
impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        eprintln!("Database error: {err}");
        ApiError::Internal("Database error".into())
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
            ErrorNumber::NoNetwork => ApiError::NotFound("Network not found".into()),
            ErrorNumber::NoStoragePool => ApiError::NotFound("Storage pool not found".into()),
            ErrorNumber::NoStorageVolume => ApiError::NotFound("Volume not found".into()),
            ErrorNumber::StorageVolExist => {
                ApiError::Conflict("A volume with this name already exists".into())
            }
            ErrorNumber::DomExist => {
                ApiError::Conflict("A VM with this name or UUID already exists".into())
            }
            ErrorNumber::InvalidArg => ApiError::BadRequest("Invalid argument".into()),
            ErrorNumber::OperationTimeout => {
                ApiError::GatewayTimeout("The hypervisor timed out".into())
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_variant_maps_to_its_http_status() {
        let cases = [
            (ApiError::Unauthorized("m".into()), StatusCode::UNAUTHORIZED),
            (ApiError::Forbidden("m".into()), StatusCode::FORBIDDEN),
            (
                ApiError::TooManyRequests("m".into()),
                StatusCode::TOO_MANY_REQUESTS,
            ),
            (ApiError::NotFound("m".into()), StatusCode::NOT_FOUND),
            (ApiError::Conflict("m".into()), StatusCode::CONFLICT),
            (ApiError::BadRequest("m".into()), StatusCode::BAD_REQUEST),
            (ApiError::BadGateway("m".into()), StatusCode::BAD_GATEWAY),
            (
                ApiError::GatewayTimeout("m".into()),
                StatusCode::GATEWAY_TIMEOUT,
            ),
            (
                ApiError::Internal("m".into()),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];
        for (error, status) in cases {
            assert_eq!(error.into_response().status(), status);
        }
    }

    #[tokio::test]
    async fn the_body_carries_the_message() {
        let response = ApiError::NotFound("VM not found".into()).into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "error");
        assert_eq!(body["content"], "VM not found");
    }
}
