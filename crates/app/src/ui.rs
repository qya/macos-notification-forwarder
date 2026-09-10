//! GPUI presentation layer ().
//!
//! Per , GPUI stays a **thin layer over the notification engine**:
//! the engine owns detection/parsing/dedup/webhook, and this module only
//! mirrors a [`StatusSnapshot`] into native views.
//!
//! The real GPUI application compiles behind the `gpui-ui` feature
//! (`gpui = "=0.2.2"`, pinned exactly per ). The default build stays
//! headless so the engine (the actual technical risk) can be built, tested,
//! and shipped without pulling the GPU UI stack.

use nf_notification::Notification;

/// Point-in-time view-model rendered by both the CLI status screen and the
/// future GPUI views. GPUI views subscribe to this; they never touch AX.
#[derive(Debug, Clone, Default)]
pub struct StatusSnapshot {
    pub monitoring: bool,
    pub accessibility: &'static str,
    pub notification_center: &'static str,
    pub webhook: &'static str,
    pub notifications_today: u64,
    pub forwarded: u64,
    pub duplicates: u64,
    pub filtered: u64,
    pub recent: Vec<Notification>,
    pub webhook_url_redacted: String,
}

impl StatusSnapshot {
    /// Text rendering used by `--status` and logs (mirrors  layout).
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Notification Forwarder\n");
        out.push_str(&format!(
            "{}\n",
            if self.monitoring {
                "● Monitoring"
            } else {
                "○ Paused"
            }
        ));
        out.push_str(&format!(
            "Accessibility     {}\nNotification      {}\nWebhook           {}\n",
            self.accessibility, self.notification_center, self.webhook
        ));
        out.push_str(&format!(
            "\nNotifications today\n{}\n(forwarded {}, duplicates {}, filtered {})\n",
            self.notifications_today, self.forwarded, self.duplicates, self.filtered
        ));
        out.push_str("\nRecent\n");
        for n in self.recent.iter().take(5) {
            out.push_str(&format!("\n{}\n{}\n{}\n", n.app_name, n.title, n.message));
        }
        out
    }

    /// Menu-bar text () for the tray tooltip / `--menu` output.
    pub fn render_menu(&self) -> String {
        format!(
            "Notification Forwarder\n\n{} Monitoring\n\nNotifications: {}\nWebhook: {}\n\nOpen\nSettings\nLogs\n\nQuit\n",
            if self.monitoring { "●" } else { "○" },
            self.notifications_today,
            self.webhook
        )
    }
}

/// Entry point for the GPUI application. Only available with `--features gpui-ui`
/// (requires full Xcode: gpui compiles Metal shaders via `xcrun metal`).
#[cfg(feature = "gpui-ui")]
pub fn run_gpui_app(initial: StatusSnapshot) {
    use gpui::{
        div, px, rgb, size, App, AppContext as _, Application, Context, IntoElement, ParentElement,
        Render, Styled, Window, WindowBounds, WindowOptions,
    };

    struct RootView {
        snapshot: StatusSnapshot,
    }

    impl Render for RootView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let s = &self.snapshot;
            let status = format!(
                "{} Monitoring · {} today ({} forwarded)",
                if s.monitoring { "●" } else { "○" },
                s.notifications_today,
                s.forwarded
            );
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_4()
                .bg(rgb(0x1e1e1e))
                .text_color(rgb(0xffffff))
                .child(div().text_lg().child("Notification Forwarder".to_string()))
                .child(div().child(status))
                .child(
                    div().flex().flex_col().gap_1().children(
                        s.recent
                            .iter()
                            .take(8)
                            .map(|n| {
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(div().child(n.app_name.clone()))
                                    .child(div().child(n.title.clone()))
                                    .child(div().child(n.message.clone()))
                            })
                            .collect::<Vec<_>>(),
                    ),
                )
                .min_w(px(380.0))
        }
    }

    Application::new().run(move |cx: &mut App| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(420.0), px(640.0)), cx)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| RootView {
                    snapshot: initial.clone(),
                })
            },
        )
        .expect("open window");
    });
}
