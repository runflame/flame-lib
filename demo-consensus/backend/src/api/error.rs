use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::error::DemoError;

pub(super) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetails,
}

#[derive(Serialize)]
struct ErrorDetails {
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        let (status, code) = match error.downcast_ref::<DemoError>() {
            Some(DemoError::InvalidRequest(_)) => (StatusCode::BAD_REQUEST, "invalid_request"),
            Some(DemoError::NotFound(_)) => (StatusCode::NOT_FOUND, "not_found"),
            Some(DemoError::Unavailable(_)) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Some(DemoError::Timeout(_)) => (StatusCode::GATEWAY_TIMEOUT, "timeout"),
            None => (StatusCode::INTERNAL_SERVER_ERROR, "operation_failed"),
        };
        if status.is_server_error() {
            tracing::error!(error = %format!("{error:#}"), "demo operation failed");
        }
        Self::new(status, code, format!("{error:#}"))
    }
}

impl From<JsonRejection> for ApiError {
    fn from(error: JsonRejection) -> Self {
        Self::new(error.status(), "invalid_json", error.body_text())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: ErrorDetails {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}
