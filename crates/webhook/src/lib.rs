//! Webhook pipeline: Filter → Deduplication → Queue → HTTP POST → Retry
//! ().

pub mod client;
pub mod queue;
pub mod retry;

pub use client::{validate_headers, WebhookClient, WebhookError, WebhookPayload};
pub use queue::WebhookQueue;
pub use retry::{backoff_for_attempt, should_retry};
