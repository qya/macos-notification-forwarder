//! Retry policy ().
//!
//! ```text
//! Attempt 1 → 1s → Attempt 2 → 3s → Attempt 3   (max 3 attempts)
//! ```
//! Never retry: 400, 401, 403, 404. Retry: 408, 429, 5xx, timeouts,
//! connection errors.

use std::time::Duration;

/// Backoff before attempt `attempt` (1-based: attempt 1 = first try).
/// Attempt 1 → 0s (send immediately), attempt 2 → 1s, attempt 3 → 3s.
pub fn backoff_for_attempt(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::from_secs(0),
        2 => Duration::from_secs(1),
        _ => Duration::from_secs(3),
    }
}

/// Whether an HTTP status code is worth retrying.
pub fn should_retry(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Whether a transport error is transient (timeout / connection refused…).
pub fn should_retry_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body() || error.is_decode()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_match_expected_policy() {
        assert_eq!(backoff_for_attempt(1), Duration::from_secs(0));
        assert_eq!(backoff_for_attempt(2), Duration::from_secs(1));
        assert_eq!(backoff_for_attempt(3), Duration::from_secs(3));
    }

    #[test]
    fn do_not_retry_client_errors() {
        for status in [400, 401, 403, 404] {
            assert!(!should_retry(status), "{status} must not retry");
        }
    }

    #[test]
    fn retry_transient() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(should_retry(status), "{status} must retry");
        }
    }
}
