//! Banner parser ().
//!
//! Priority:
//! 1. Structured AX children (ordered static texts)
//! 2. AXTitle
//! 3. AXDescription
//! 4. Other accessible text

use crate::model::Notification;

/// Raw fields extracted from one `AXNotificationCenterBanner` element by the
/// accessibility scanner. Plain data — no AX types here, keeping this crate
/// testable without macOS permissions.
#[derive(Debug, Clone, Default)]
pub struct BannerFields {
    /// `AXIdentifier`, if exposed.
    pub identifier: Option<String>,
    /// Best guess at the owning application (window title, group label…).
    pub app_name_hint: Option<String>,
    /// `AXTitle` on the banner element.
    pub title_attr: Option<String>,
    /// `AXDescription` on the banner element.
    pub description_attr: Option<String>,
    /// Ordered text from `AXStaticText` / `AXTextField` descendants.
    pub static_texts: Vec<String>,
}

/// Parse raw banner fields into a [`Notification`].
///
/// Returns `None` when there is nothing worth forwarding (all fields empty).
pub fn parse_banner_fields(fields: &BannerFields) -> Option<Notification> {
    let texts: Vec<&str> = fields
        .static_texts
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    // 1. Structured children: first text = title, rest = body.
    //    Many banners expose [app, title, body] or [title, body].
    let (mut title, mut message) = match texts.as_slice() {
        [] => (None, None),
        [single] => (Some(single.to_string()), None),
        [first, rest @ ..] => (Some(first.to_string()), Some(rest.join("\n"))),
    };

    // 2. AXTitle fills a missing title.
    if title.as_ref().is_none_or(|t| t.is_empty()) {
        if let Some(t) = fields
            .title_attr
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            title = Some(t.to_string());
        }
    }

    // 3. AXDescription fills a missing body.
    if message.as_ref().is_none_or(|m| m.is_empty()) {
        if let Some(d) = fields
            .description_attr
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            // Avoid duplicating the title into the body.
            if Some(d) != title.as_deref() {
                message = Some(d.to_string());
            }
        }
    }

    let title = title.filter(|t| !t.is_empty())?;
    let message = message.unwrap_or_default();

    // App name: explicit hint wins; otherwise first structured text may be the
    // app label (e.g. [app, title, body]) — detect the 3-text shape.
    let mut app_name = fields
        .app_name_hint
        .as_deref()
        .and_then(|hint| parse_app_name_hint(hint, &texts));

    let mut final_title = title;
    let mut final_message = message;
    if app_name.is_none() && texts.len() >= 3 {
        app_name = Some(texts[0].to_string());
        final_title = texts[1].to_string();
        final_message = texts[2..].join("\n");
        if final_title.is_empty() {
            return None;
        }
    }
    let app_name = app_name
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown".to_string());

    // Guard: a banner whose title equals its only text and has no body is
    // still a valid notification (e.g. title-only banners).
    let mut notification = Notification::new(app_name, final_title, final_message);
    if let Some(id) = fields
        .identifier
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        notification = notification.with_id(id.trim());
    }
    // Drop empty shells (AT-02 negative case).
    if notification.title.is_empty() && notification.message.is_empty() {
        return None;
    }
    Some(notification)
}

fn is_generic_app_name(value: &str) -> bool {
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

fn parse_app_name_hint(hint: &str, texts: &[&str]) -> Option<String> {
    // Notification Center may prefix labels with invisible bidi formatting
    // marks and expose a spoken summary such as:
    // "\u{200e}WhatsApp, Niles, Hello".
    let cleaned: String = hint
        .chars()
        .filter(|character| !is_bidi_mark(*character))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || is_generic_app_name(cleaned) {
        return None;
    }

    if let (Some(title), Some((candidate, summary))) = (texts.first(), cleaned.split_once(',')) {
        if summary.trim_start().starts_with(*title) {
            let candidate = candidate.trim();
            if !candidate.is_empty() && !is_generic_app_name(candidate) {
                return Some(candidate.to_string());
            }
        }
    }

    Some(cleaned.to_string())
}

fn is_bidi_mark(character: char) -> bool {
    matches!(
        character,
        '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
            | '\u{feff}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_example() {
        // AXNotificationCenterBanner
        // ├── title = "Niles 💟"
        // └── body  = "AAA"
        let fields = BannerFields {
            app_name_hint: Some("WhatsApp".to_string()),
            static_texts: vec!["Niles 💟".to_string(), "AAA".to_string()],
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.app_name, "WhatsApp");
        assert_eq!(n.title, "Niles 💟");
        assert_eq!(n.message, "AAA");
    }

    #[test]
    fn falls_back_to_ax_title_and_description() {
        let fields = BannerFields {
            app_name_hint: Some("Messages".to_string()),
            title_attr: Some("John".to_string()),
            description_attr: Some("Hello".to_string()),
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.title, "John");
        assert_eq!(n.message, "Hello");
    }

    #[test]
    fn three_text_shape_extracts_app_name() {
        let fields = BannerFields {
            static_texts: vec![
                "Telegram".to_string(),
                "Alice".to_string(),
                "Where are you?".to_string(),
            ],
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.app_name, "Telegram");
        assert_eq!(n.title, "Alice");
    }

    #[test]
    fn generic_container_hint_does_not_override_mobile_app() {
        let fields = BannerFields {
            app_name_hint: Some("Notification Center".to_string()),
            static_texts: vec![
                "Booking".to_string(),
                "Booking Confirmed".to_string(),
                "Your booking has been confirmed by Booking.".to_string(),
            ],
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.app_name, "Booking");
        assert_eq!(n.title, "Booking Confirmed");
    }

    #[test]
    fn generic_container_hint_is_never_reported_as_originating_app() {
        let fields = BannerFields {
            app_name_hint: Some("Notification Center".to_string()),
            static_texts: vec![
                "Booking Confirmed".to_string(),
                "Your booking has been confirmed by Booking.".to_string(),
            ],
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.app_name, "Unknown");
        assert_eq!(n.title, "Booking Confirmed");
    }

    #[test]
    fn extracts_app_from_notification_center_spoken_summary() {
        let fields = BannerFields {
            app_name_hint: Some("\u{200e}WhatsApp, Niles 💟, Miss you".to_string()),
            static_texts: vec!["Niles 💟".to_string(), "Miss you".to_string()],
            ..Default::default()
        };
        let n = parse_banner_fields(&fields).expect("parses");
        assert_eq!(n.app_name, "WhatsApp");
        assert_eq!(n.title, "Niles 💟");
        assert_eq!(n.message, "Miss you");
    }

    #[test]
    fn empty_banner_is_ignored() {
        assert!(parse_banner_fields(&BannerFields::default()).is_none());
    }
}
