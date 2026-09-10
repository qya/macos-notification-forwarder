//! `AXObserver` engine ().
//!
//! Preferred flow:
//! ```text
//! AXObserver → AX event → short debounce → Notification Center scan
//! ```
//! No continuous polling: the process idles until macOS delivers an event.
//! If Notification Center restarts, the engine rediscovers the PID and
//! resubscribes automatically.

use std::time::Duration;

use axuielement::{
    async_api::AXNotificationStream,
    ax_notification::{
        AXUI_ELEMENT_DESTROYED_NOTIFICATION, AX_CREATED_NOTIFICATION,
        AX_LAYOUT_CHANGED_NOTIFICATION, AX_TITLE_CHANGED_NOTIFICATION,
        AX_VALUE_CHANGED_NOTIFICATION, AX_WINDOW_CREATED_NOTIFICATION,
    },
    AXUIElement,
};
use tracing::{debug, error, info, warn};

use crate::process::{find_notification_center_pid, wait_for_notification_center};
use crate::scanner::{scan_for_banners, BannerSnapshot};

/// Events emitted by the engine toward the processing pipeline.
#[derive(Debug)]
pub enum MonitorEvent {
    /// A scan completed; carries whatever banners are currently visible.
    /// The pipeline diffs content fingerprints, so re-emitting visible
    /// banners is safe ().
    Banners(Vec<BannerSnapshot>),
    /// Notification Center PID changed (restart detected + reconnected).
    Reconnected { pid: i32 },
    /// Accessibility permission missing (AT-07).
    PermissionRevoked,
}

/// AX notifications worth subscribing to. The exact set is validated per
/// macOS version (); all are best-effort — a rejected registration
/// logs a warning and monitoring continues with the rest.
const SUBSCRIPTIONS: &[&str] = &[
    AX_CREATED_NOTIFICATION,
    AXUI_ELEMENT_DESTROYED_NOTIFICATION,
    AX_VALUE_CHANGED_NOTIFICATION,
    AX_TITLE_CHANGED_NOTIFICATION,
    AX_CREATED_NOTIFICATION,
    AX_WINDOW_CREATED_NOTIFICATION,
    AX_LAYOUT_CHANGED_NOTIFICATION,
];

pub struct ObserverEngine {
    pub debounce: Duration,
}

impl ObserverEngine {
    pub fn new(debounce: Duration) -> Self {
        Self { debounce }
    }

    /// Main loop. Only returns on fatal error; reconnects internally.
    pub async fn run(&self, tx: tokio::sync::mpsc::Sender<MonitorEvent>) {
        if !crate::accessibility_trusted() {
            error!("accessibility permission not granted (AT-07)");
            let _ = tx.send(MonitorEvent::PermissionRevoked).await;
            return;
        }

        loop {
            let pid = match find_notification_center_pid() {
                Some(pid) => pid,
                None => {
                    warn!("notification Center not found; waiting for it to appear");
                    wait_for_notification_center().await
                }
            };
            info!(pid, "AX observer starting");
            match self.monitor_pid(pid, &tx).await {
                StopReason::StreamEnded => {
                    warn!(
                        "AX stream ended (Notification Center may have restarted); rediscovering"
                    );
                }
                StopReason::PermissionRevoked => {
                    let _ = tx.send(MonitorEvent::PermissionRevoked).await;
                    return;
                }
            }
            // Back off briefly so a crash-looping NC doesn't spin us.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    async fn monitor_pid(
        &self,
        pid: i32,
        tx: &tokio::sync::mpsc::Sender<MonitorEvent>,
    ) -> StopReason {
        let Some(app) = AXUIElement::from_pid(pid) else {
            warn!(pid, "AXUIElement::from_pid failed; will rediscover");
            return StopReason::StreamEnded;
        };
        let _ = app.set_timeout(2.0);

        let stream = match AXNotificationStream::subscribe_many(&app, SUBSCRIPTIONS, 128) {
            Ok(stream) => stream,
            Err(e) => {
                // Fall back to the two guaranteed notifications if the full
                // set is rejected on this macOS version.
                warn!(error = ?e, "full AX subscription failed; retrying with core set");
                let core = [AX_CREATED_NOTIFICATION, AXUI_ELEMENT_DESTROYED_NOTIFICATION];
                match AXNotificationStream::subscribe_many(&app, &core, 128) {
                    Ok(stream) => stream,
                    Err(e) => {
                        error!(error = ?e, "AX subscription failed entirely");
                        return StopReason::StreamEnded;
                    }
                }
            }
        };

        info!("AXObserver created; listening for Accessibility events");
        // Initial scan so already-visible banners are picked up at launch.
        self.scan_and_emit(&app, tx).await;

        let stream = stream;
        // Debounce: collapse bursts of AX events into one scan ().
        loop {
            // Wait for the next event, then drain + debounce. The stream
            // yields `AXObserverEvent` directly (`None` = closed).
            let Some(ev) = stream.next().await else {
                return StopReason::StreamEnded;
            };
            debug!(notification = %ev.notification, "AX event received");
            if !crate::accessibility_trusted() {
                return StopReason::PermissionRevoked;
            }
            // Drain coalesced events already buffered.
            while stream.try_next().is_some() {}
            // Short debounce before the targeted scan.
            tokio::time::sleep(self.debounce).await;
            // If NC died mid-debounce, `scan_and_emit` yields nothing and the
            // next `stream.next()` will end the loop → reconnect.
            if find_notification_center_pid() != Some(pid) {
                warn!("Notification Center PID changed; reconnecting");
                let _ = tx.send(MonitorEvent::Reconnected { pid: -1 }).await;
                return StopReason::StreamEnded;
            }
            self.scan_and_emit(&app, tx).await;
        }
    }

    async fn scan_and_emit(&self, app: &AXUIElement, tx: &tokio::sync::mpsc::Sender<MonitorEvent>) {
        // AX calls are blocking; run them on a blocking thread. The element
        // is Send + Sync so we can move a clone.
        let app = app.clone();
        let banners = tokio::task::spawn_blocking(move || scan_for_banners(&app))
            .await
            .unwrap_or_default();
        debug!(count = banners.len(), "scan complete");
        let _ = tx.send(MonitorEvent::Banners(banners)).await;
    }
}

enum StopReason {
    StreamEnded,
    PermissionRevoked,
}

/// One-shot scan used by `--test-detection` and diagnostics.
pub fn scan_once() -> Result<Vec<BannerSnapshot>, String> {
    if !crate::accessibility_trusted() {
        return Err("accessibility permission not granted".to_string());
    }
    let pid = find_notification_center_pid()
        .ok_or_else(|| "Notification Center not found".to_string())?;
    let app = AXUIElement::from_pid(pid)
        .ok_or_else(|| "failed to create AXUIElement for Notification Center".to_string())?;
    Ok(scan_for_banners(&app))
}
