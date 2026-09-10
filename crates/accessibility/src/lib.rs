//! Accessibility engine ().
//!
//! Flow: permission check → Notification Center discovery → `AXUIElement` →
//! `AXObserver` → debounced targeted scan → banner snapshots → processing
//! pipeline. Reconnects automatically when Notification Center restarts.

pub mod element;
pub mod observer;
pub mod process;
pub mod scanner;

pub use element::{collect_texts, dump_tree, get_string};
pub use observer::{scan_once, MonitorEvent, ObserverEngine};
pub use process::find_notification_center_pid;
pub use scanner::{scan_for_banners, BannerSnapshot};

/// Current Accessibility TCC state (`AXIsProcessTrusted`, not the deprecated
/// `AXAPIEnabled` wrapper).
pub fn accessibility_trusted() -> bool {
    axuielement::is_process_trusted()
}

/// Ask macOS to show the system Accessibility prompt if the process is not yet trusted.
pub fn prompt_accessibility() -> bool {
    axuielement::is_process_trusted_with_prompt()
}
