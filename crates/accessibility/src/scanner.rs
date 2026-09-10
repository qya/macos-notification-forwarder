//! Banner scanner ().
//!
//! Primary signal: `AXSubrole == AXNotificationCenterBanner`.
//! The scanner never relies on that signal exclusively: it also accepts
//! fallback heuristics (groups/rows carrying notification-like identifiers)
//! because the AX structure varies across macOS versions.

use axuielement::AXUIElement;
use nf_notification::BannerFields;

use crate::element::{collect_texts, get_string};

/// One detected banner, still in raw AX-extracted form.
#[derive(Debug, Clone)]
pub struct BannerSnapshot {
    /// `AXIdentifier`, when exposed. May repeat across scans — identity is
    /// content-based (), this is only a hint for debugging.
    pub ax_id: Option<String>,
    /// Best-effort owning-app guess from surrounding chrome.
    pub app_name_hint: Option<String>,
    pub title_attr: Option<String>,
    pub description_attr: Option<String>,
    /// Ordered static texts inside the banner.
    pub static_texts: Vec<String>,
}

impl BannerSnapshot {
    pub fn into_fields(self) -> BannerFields {
        BannerFields {
            identifier: self.ax_id,
            app_name_hint: self.app_name_hint,
            title_attr: self.title_attr,
            description_attr: self.description_attr,
            static_texts: self.static_texts,
        }
    }
}

/// Scan the Notification Center AX hierarchy for banners.
pub fn scan_for_banners(app: &AXUIElement) -> Vec<BannerSnapshot> {
    let mut banners = Vec::new();
    let mut visited = 0usize;
    scan_inner(app, 0, &mut banners, &mut visited, None);
    banners
}

fn scan_inner(
    element: &AXUIElement,
    depth: usize,
    banners: &mut Vec<BannerSnapshot>,
    visited: &mut usize,
    app_hint: Option<String>,
) {
    if depth > 14 || *visited > 800 || banners.len() >= 50 {
        return;
    }
    *visited += 1;

    let subrole = get_string(element, "AXSubrole");
    let role = get_string(element, "AXRole").unwrap_or_default();

    // Track app-name chrome: window titles / group descriptions above banners.
    // The outer window is commonly titled "Notification Center"; that is the
    // host process, not the app that originated an iPhone notification.
    let mut hint = app_hint.filter(|value| !is_generic_app_hint(value));
    if role == "AXWindow" || role == "AXGroup" {
        if let Some(candidate) =
            get_string(element, "AXTitle").or_else(|| get_string(element, "AXDescription"))
        {
            if hint.is_none() && !is_generic_app_hint(&candidate) {
                hint = Some(candidate);
            }
        }
    }

    let is_banner_subrole = subrole.as_deref() == Some("AXNotificationCenterBanner");
    if is_banner_subrole {
        banners.push(snapshot_banner(element, hint.clone()));
        // Still descend: stacked banners can nest.
    } else if depth >= 3 && is_fallback_banner(element, &role) {
        banners.push(snapshot_banner(element, hint.clone()));
    }

    let Ok(children) = element.children() else {
        return;
    };
    for child in children.iter().take(60) {
        scan_inner(child, depth + 1, banners, visited, hint.clone());
    }
}

/// Fallback heuristic when the banner subrole is absent on this macOS build:
/// a group/row with a notification-ish identifier.
fn is_fallback_banner(element: &AXUIElement, role: &str) -> bool {
    let identifier = get_string(element, "AXIdentifier")
        .unwrap_or_default()
        .to_lowercase();
    let looks_like_banner = identifier.contains("banner") || identifier.contains("notification");
    looks_like_banner && (role == "AXGroup" || role == "AXRow" || role.is_empty())
}

fn snapshot_banner(element: &AXUIElement, app_hint: Option<String>) -> BannerSnapshot {
    BannerSnapshot {
        ax_id: get_string(element, "AXIdentifier"),
        app_name_hint: app_hint,
        title_attr: get_string(element, "AXTitle"),
        description_attr: get_string(element, "AXDescription"),
        static_texts: collect_texts(element, 4),
    }
}

fn is_generic_app_hint(value: &str) -> bool {
    let normalized: String = value
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "notificationcenter"
            | "notificationcentre"
            | "notifications"
            | "notification"
            | "controlcenter"
    )
}

#[cfg(test)]
mod tests {
    use super::is_generic_app_hint;

    #[test]
    fn rejects_system_container_as_app_hint() {
        assert!(is_generic_app_hint("Notification Center"));
        assert!(is_generic_app_hint("NotificationCenter"));
        assert!(!is_generic_app_hint("Booking"));
        assert!(!is_generic_app_hint("Jago"));
    }
}
