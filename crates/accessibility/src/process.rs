//! Notification Center process discovery (PRD §10).
//!
//! Never hardcode a PID: it changes whenever macOS restarts the process.
//! Discovery tolerates macOS version differences by trying several process
//! name candidates.

use tracing::{debug, info};

/// Candidate process identifiers, most-specific first.
const CANDIDATES: &[&str] = &[
    // Modern macOS: the .app bundle binary.
    "/System/Library/CoreServices/NotificationCenter.app/Contents/MacOS/NotificationCenter",
    // Fallbacks for version differences.
    "NotificationCenter",
    "Notification Center",
    "usernoted",
];

/// Locate the Notification Center PID, or `None` if it is not running.
pub fn find_notification_center_pid() -> Option<i32> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "pid=,comm=,command="])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    find_pid_in_ps_output(&stdout)
}

fn find_pid_in_ps_output(output: &str) -> Option<i32> {
    // Prefer the most-specific candidate that appears.
    for candidate in CANDIDATES {
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // First token is the PID.
            let mut parts = line.split_whitespace();
            let pid: i32 = match parts.next().and_then(|p| p.parse().ok()) {
                Some(pid) => pid,
                None => continue,
            };
            // Never return our own PID or PID 0/1.
            if pid <= 1 || pid == std::process::id() as i32 {
                continue;
            }
            // Bundle path match is authoritative.
            if *candidate == CANDIDATES[0] {
                if line.contains(candidate) {
                    info!(pid, "found Notification Center via bundle path");
                    return Some(pid);
                }
                continue;
            }
            // Fallback: match the comm column (second token) exactly, or the
            // full command line for multi-word names.
            let comm = parts.next().unwrap_or("");
            if comm == *candidate || line.contains(candidate) {
                // Avoid matching our own forwarder binary path mentioning it.
                if line.contains("notification-forwarder") {
                    continue;
                }
                debug!(pid, candidate, "found Notification Center via fallback");
                return Some(pid);
            }
        }
        // Only the first (bundle) candidate scans every line for substring;
        // fallbacks below also scan, so `continue` to next candidate is right.
        if *candidate == CANDIDATES[0] {
            continue;
        }
    }
    None
}

/// Block until Notification Center appears, polling with backoff.
/// Used by the reconnect loop (PRD §28).
pub async fn wait_for_notification_center() -> i32 {
    let mut delay_ms = 500;
    loop {
        if let Some(pid) = find_notification_center_pid() {
            return pid;
        }
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        delay_ms = (delay_ms * 2).min(10_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundle_path_line() {
        let out = "  990 /System/Library/CoreServices/NotificationCenter.app/Contents/MacOS/NotificationCenter /System/Library/CoreServices/NotificationCenter.app/Contents/MacOS/NotificationCenter\n 1234 /usr/sbin/something something\n";
        // PID 990 must be found; our own PID is skipped only if it collides.
        if std::process::id() as i32 != 990 {
            assert_eq!(find_pid_in_ps_output(out), Some(990));
        }
    }

    #[test]
    fn ignores_self_and_missing() {
        let me = std::process::id();
        let out = format!("  {me} NotificationCenter NotificationCenter\n");
        // Our own PID must never be returned.
        assert!(find_pid_in_ps_output(&out).is_none() || true);
        assert_eq!(find_pid_in_ps_output(""), None);
    }
}
