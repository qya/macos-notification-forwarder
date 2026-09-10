//! Internal notification model (PRD §7).

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

/// Internal Rust structure for a detected notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notification {
    /// AX-derived stable identifier, if the banner exposed one.
    /// `None` when Notification Center reused an element without an ID —
    /// see PRD §12: identity is content-based, never ID-based alone.
    pub id: Option<String>,
    pub app_name: String,
    pub title: String,
    pub message: String,
    pub timestamp: DateTime<Local>,
}

impl Notification {
    pub fn new(
        app_name: impl Into<String>,
        title: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: None,
            app_name: app_name.into(),
            title: title.into(),
            message: message.into(),
            timestamp: Local::now(),
        }
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.id = if id.is_empty() { None } else { Some(id) };
        self
    }

    /// Content fingerprint per PRD §15:
    /// `SHA256(app_name + "\0" + title + "\0" + message)`.
    pub fn fingerprint(&self) -> String {
        crate::dedup::fingerprint(&self.app_name, &self.title, &self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_stable() {
        let a = Notification::new("WhatsApp", "Niles 💟", "AAA");
        let b = Notification::new("WhatsApp", "Niles 💟", "AAA");
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn example_from_prd() {
        let n = Notification::new("WhatsApp", "Niles 💟", "AAA");
        assert_eq!(n.app_name, "WhatsApp");
        assert_eq!(n.title, "Niles 💟");
        assert_eq!(n.message, "AAA");
    }
}
