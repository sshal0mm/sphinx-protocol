//! HTTP rendering of catalog errors. The body is always the shared
//! `ErrorReport`, so agents parse one shape everywhere.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use sphinx_protocol::{Code, Retry, SphinxError};

/// An error returned by an API handler.
#[derive(Debug)]
pub struct ApiError(pub SphinxError);

impl From<SphinxError> for ApiError {
    fn from(e: SphinxError) -> Self {
        Self(e)
    }
}

/// HTTP status for an error: the retry class decides, with not-found and
/// upstream failures mapped explicitly.
pub fn status_for(e: &SphinxError) -> StatusCode {
    match e.code() {
        Code::JobNotFound | Code::ScoreNoSubmission | Code::ApiCodeNotFound => {
            StatusCode::NOT_FOUND
        }
        Code::RelayRpcFailed => StatusCode::BAD_GATEWAY,
        Code::ApiRateLimited => StatusCode::TOO_MANY_REQUESTS,
        Code::GraderCodeTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        _ => match e.retry_class() {
            Retry::FixInput => StatusCode::UNPROCESSABLE_ENTITY,
            Retry::Wait | Retry::Never => StatusCode::CONFLICT,
            Retry::Transient => StatusCode::SERVICE_UNAVAILABLE,
        },
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (status_for(&self.0), Json(self.0.to_report())).into_response();
        if let Some(secs) = self.0.context().get("retry_after_secs") {
            if let Ok(value) = HeaderValue::from_str(secs) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
