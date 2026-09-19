//! Shared HTTP middleware for ARMIN servers.
//!
//! `require_bearer` verifies the `Authorization: Bearer <token>` header
//! against the configured token. When no token is configured (server bound
//! to loopback without an auth flag) all requests pass through, so local
//! development and the demo frontend keep working unchanged.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub const AUTH_HEADER: &str = "authorization";

pub type AuthToken = Option<Arc<String>>;

/// Build the state for `middleware::from_fn_with_state`.
pub fn optional_token(token: Option<String>) -> AuthToken {
    token.filter(|t| !t.is_empty()).map(Arc::new)
}

pub async fn require_bearer(
    State(expected): State<AuthToken>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = expected else {
        return next.run(req).await;
    };

    match bearer_token(req.headers()) {
        Some(token) if constant_time_eq(token, expected.as_str()) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            "missing or invalid bearer token",
        )
            .into_response(),
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTH_HEADER)?.to_str().ok()?;
    value.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}

/// Comparison that does not short-circuit on the first differing byte.
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_std_semantics() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn optional_token_filters_empty() {
        assert!(optional_token(None).is_none());
        assert!(optional_token(Some(String::new())).is_none());
        assert!(optional_token(Some("secret".into())).is_some());
    }
}
