//! Bounded async webhook dispatch queue (PRD §18).
//!
//! The notification engine pushes payloads without blocking on the network;
//! a single background worker POSTs them with retry and records outcomes.

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::client::{WebhookClient, WebhookPayload};

/// Outcome counters surfaced in the status UI.
#[derive(Debug, Default, Clone)]
pub struct QueueStats {
    pub enqueued: u64,
    pub delivered: u64,
    pub failed: u64,
    pub dropped_full: u64,
}

/// Bounded queue feeding one POST worker.
pub struct WebhookQueue {
    tx: mpsc::Sender<WebhookPayload>,
    stats: std::sync::Arc<tokio::sync::Mutex<QueueStats>>,
}

impl WebhookQueue {
    pub fn spawn(client: WebhookClient, capacity: usize) -> Self {
        let (tx, mut rx) = mpsc::channel::<WebhookPayload>(capacity.max(8));
        let stats = std::sync::Arc::new(tokio::sync::Mutex::new(QueueStats::default()));
        let worker_stats = stats.clone();
        tokio::spawn(async move {
            while let Some(payload) = rx.recv().await {
                match client.send(&payload).await {
                    Ok(status) => {
                        info!(status, app = %payload.app_name, "webhook delivered");
                        worker_stats.lock().await.delivered += 1;
                    }
                    Err(e) => {
                        warn!(error = %e, app = %payload.app_name, "webhook failed after retries");
                        worker_stats.lock().await.failed += 1;
                    }
                }
            }
        });
        Self { tx, stats }
    }

    /// Enqueue without awaiting delivery. `try_send` so a slow network can
    /// never back-pressure the AX pipeline; drops are counted, not silent.
    pub async fn enqueue(&self, payload: WebhookPayload) {
        match self.tx.try_send(payload) {
            Ok(()) => {
                self.stats.lock().await.enqueued += 1;
            }
            Err(_) => {
                warn!("webhook queue full; dropping notification");
                self.stats.lock().await.dropped_full += 1;
            }
        }
    }

    pub async fn stats(&self) -> QueueStats {
        self.stats.lock().await.clone()
    }
}
