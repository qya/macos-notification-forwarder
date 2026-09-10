//! Local configuration file handling (PRD §5, §22, §32).
//!
//! Config lives at:
//! `~/Library/Application Support/NotificationForwarder/config.json`
//! with fallback to `~/.config/notification-forwarder/config.json`.
//! File is created with `0o600` permissions so webhook secrets are not
//! world-readable.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How application filtering behaves (PRD §16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Forward from every application.
    #[default]
    All,
    /// Forward only from `allowed_apps`.
    Selected,
}

/// Full application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Destination webhook URL (POST JSON, PRD §17).
    #[serde(default)]
    pub webhook_url: String,
    /// Additional HTTP headers included with every webhook request.
    ///
    /// Values may contain credentials, so they must never be written to logs.
    #[serde(default)]
    pub webhook_headers: BTreeMap<String, String>,
    /// Master kill-switch (Settings → Enable forwarding).
    #[serde(default = "default_true")]
    pub forwarding_enabled: bool,
    /// Verbose AX logging (Settings → Enable diagnostic logging).
    #[serde(default)]
    pub diagnostic_logging: bool,
    /// Launch at login (PRD §33). Best-effort; see README.
    #[serde(default)]
    pub start_at_login: bool,
    /// Application filter mode.
    #[serde(default)]
    pub filter_mode: FilterMode,
    /// Apps to forward when `filter_mode == Selected`.
    #[serde(default)]
    pub allowed_apps: HashSet<String>,
    /// Deduplication TTL in seconds (PRD §15: 5–30 min).
    #[serde(default = "default_dedup_ttl_secs")]
    pub dedup_ttl_secs: u64,
    /// Max dedup fingerprints retained (bounded cache, PRD §15).
    #[serde(default = "default_dedup_capacity")]
    pub dedup_capacity: usize,
    /// Debounce between AX event and targeted scan, ms (PRD §30).
    #[serde(default = "default_debounce_ms")]
    pub debounce_ms: u64,
    /// Max webhook attempts incl. the first try (PRD §19: 3).
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
}

fn default_true() -> bool {
    true
}

fn default_dedup_ttl_secs() -> u64 {
    900 // 15 minutes, inside the 5–30 min window.
}

fn default_dedup_capacity() -> usize {
    2000
}

fn default_debounce_ms() -> u64 {
    250
}

fn default_max_attempts() -> u32 {
    3
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            webhook_url: String::new(),
            webhook_headers: BTreeMap::new(),
            forwarding_enabled: true,
            diagnostic_logging: false,
            start_at_login: false,
            filter_mode: FilterMode::All,
            allowed_apps: HashSet::new(),
            dedup_ttl_secs: default_dedup_ttl_secs(),
            dedup_capacity: default_dedup_capacity(),
            debounce_ms: default_debounce_ms(),
            max_attempts: default_max_attempts(),
        }
    }
}

impl AppConfig {
    /// Resolve the config file path, honouring `$NOTIFORWARDER_CONFIG` override.
    pub fn config_path() -> PathBuf {
        if let Ok(override_path) = std::env::var("NOTIFORWARDER_CONFIG") {
            return PathBuf::from(override_path);
        }
        if let Ok(home) = std::env::var("HOME") {
            let mac_path = PathBuf::from(&home)
                .join("Library/Application Support/NotificationForwarder/config.json");
            // Prefer the macOS location; fall back to XDG if the parent exists.
            if mac_path.parent().is_some() {
                return mac_path;
            }
        }
        PathBuf::from("/tmp/notification-forwarder-config.json")
    }

    /// Load from disk, returning defaults when the file is missing.
    pub fn load() -> Self {
        Self::load_from(&Self::config_path())
    }

    /// Load from an explicit path.
    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Persist to disk with `0o600` permissions (PRD §32).
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&Self::config_path())
    }

    /// Persist to an explicit path with `0o600` permissions.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

/// Redact secrets from a URL for logging (PRD §32).
///
/// `https://api.example.com/webhook?token=abc` → `...?token=********`
pub fn sanitize_url_for_logging(url: &str) -> String {
    const SECRET_KEYS: &[&str] = &["token", "secret", "key", "auth", "password", "signature"];
    let Some((base, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let redacted: Vec<String> = query
        .split('&')
        .map(|pair| {
            let key = pair.split_once('=').map(|(k, _)| k).unwrap_or(pair);
            let lowered = key.to_lowercase();
            if SECRET_KEYS.iter().any(|s| lowered.contains(s)) {
                format!("{key}=********")
            } else {
                pair.to_string()
            }
        })
        .collect();
    format!("{base}?{}", redacted.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_require_custom_webhook() {
        let cfg = AppConfig::default();
        assert!(cfg.webhook_url.is_empty());
        assert_eq!(cfg.max_attempts, 3);
        assert!((300..=1800).contains(&cfg.dedup_ttl_secs));
        assert!(cfg.forwarding_enabled);
    }

    #[test]
    fn secret_redaction() {
        let out = sanitize_url_for_logging("https://api.example.com/webhook?token=abc123&foo=bar");
        assert!(out.contains("token=********"));
        assert!(out.contains("foo=bar"));
        assert!(!out.contains("abc123"));
    }

    #[test]
    fn round_trip() {
        let mut cfg = AppConfig::default();
        cfg.allowed_apps.insert("WhatsApp".to_string());
        cfg.webhook_headers
            .insert("Authorization".to_string(), "Bearer secret".to_string());
        let json = serde_json::to_string(&cfg).unwrap();
        let back: AppConfig = serde_json::from_str(&json).unwrap();
        assert!(back.allowed_apps.contains("WhatsApp"));
        assert_eq!(
            back.webhook_headers.get("Authorization"),
            Some(&"Bearer secret".to_string())
        );
    }
}
