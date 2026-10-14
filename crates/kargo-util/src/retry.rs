//! The retry policy for transient network failures, shared by Kargo's HTTP clients
//! (dependency downloads in `kargo-maven`, toolchain downloads in `kargo-toolchain`).

use std::time::Duration;

/// How many times a request that failed transiently is retried after its first attempt.
pub const MAX_RETRIES: u32 = 5;

/// Whether an HTTP status is worth sending the request again for: rate limits, request
/// timeouts and gateway errors.
pub fn is_transient_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// How long to wait before retry number `retry` (counting from 0): 250 ms, then doubling.
pub fn backoff(retry: u32) -> Duration {
    Duration::from_millis(250 << retry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_statuses() {
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(is_transient_status(status), "{status}");
        }
        for status in [200, 301, 400, 401, 403, 404, 410, 501] {
            assert!(!is_transient_status(status), "{status}");
        }
    }

    #[test]
    fn backs_off_exponentially() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(MAX_RETRIES - 1), Duration::from_millis(4000));
    }
}
