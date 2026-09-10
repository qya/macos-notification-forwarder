//! Application filtering (PRD §16, AT-08).
//!
//! ```text
//! WhatsApp → forwarded
//! Slack    → ignored      (when only WhatsApp is enabled)
//! ```

use std::collections::HashSet;

use nf_config::{AppConfig, FilterMode};

/// Runtime filter compiled from [`AppConfig`].
#[derive(Debug, Clone)]
pub struct AppFilter {
    mode: FilterMode,
    /// Lower-cased allowed app names for case-insensitive matching.
    allowed: HashSet<String>,
}

impl AppFilter {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            mode: config.filter_mode,
            allowed: config
                .allowed_apps
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
    }

    pub fn allow_all() -> Self {
        Self {
            mode: FilterMode::All,
            allowed: HashSet::new(),
        }
    }

    /// Filtering happens before webhook dispatch (PRD §16).
    pub fn allows(&self, app_name: &str) -> bool {
        match self.mode {
            FilterMode::All => true,
            FilterMode::Selected => self.allowed.contains(&app_name.to_lowercase()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mode_forwards_everything() {
        let f = AppFilter::allow_all();
        assert!(f.allows("WhatsApp"));
        assert!(f.allows("Slack"));
    }

    #[test]
    fn selected_mode_filters() {
        let config = AppConfig {
            filter_mode: FilterMode::Selected,
            allowed_apps: ["WhatsApp".to_string(), "Messages".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let f = AppFilter::from_config(&config);
        assert!(f.allows("WhatsApp"));
        assert!(f.allows("whatsapp")); // case-insensitive
        assert!(!f.allows("Slack"));
    }
}
