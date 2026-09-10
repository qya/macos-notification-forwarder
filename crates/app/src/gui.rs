//! Native macOS GUI: menu-bar tray + status window.
//!
//! Pure Rust via `objc2` — no Swift helper, no WebView, no GPUI (which needs
//! full Xcode for Metal shaders). Runs on the main thread under
//! `NSApplication.run()`; the notification engine lives on a worker thread
//! with its own tokio runtime. The two sides talk over channels:
//! [`GuiAction`] (GUI → engine) and [`GuiEvent`] (engine → GUI, drained by a
//! 0.25 s `NSTimer`).

use std::cell::{Ref, RefCell};
use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, Sender},
    Arc,
};

use nf_config::FilterMode;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSBackingStoreType, NSBorderType, NSBox, NSBoxType, NSButton, NSColor,
    NSFont, NSImage, NSMenu, NSMenuItem, NSScrollView, NSSegmentedControl, NSSegmentSwitchTracking,
    NSStatusBar, NSStatusItem, NSTextField, NSTextView, NSView, NSWindow, NSWindowButton,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    ns_string, NSArray, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
    NSTimer,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

// ---------------------------------------------------------------------------
// Engine ↔ GUI protocol
// ---------------------------------------------------------------------------

/// GUI → engine requests (menu clicks, buttons).
#[derive(Debug)]
pub enum GuiAction {
    TogglePause,
    TestWebhook,
    SetWebhookConfig {
        url: String,
        headers: Vec<(String, String)>,
    },
    SetFilter {
        mode: FilterMode,
        allowed: Vec<String>,
    },
    DumpTree,
    TestDetection,
    Quit,
}

/// One recent notification for the history view.
#[derive(Debug, Clone)]
pub struct RecentItem {
    pub app: String,
    pub title: String,
    pub message: String,
}

/// Engine → GUI snapshot (labels, history, settings echo).
#[derive(Debug, Clone)]
pub struct GuiStatus {
    pub monitoring: bool,
    pub today: u64,
    pub forwarded: u64,
    pub duplicates: u64,
    pub filtered: u64,
    pub accessibility: String,
    #[allow(dead_code)]
    pub needs_permission: bool,
    pub notification_center: String,
    pub webhook: String,
    pub recent: Vec<RecentItem>,
    pub filter_mode: FilterMode,
    pub exiting: bool,
}

/// Engine → GUI events.
#[derive(Debug, Clone)]
pub enum GuiEvent {
    /// One realtime console line.
    Log(String),
    /// Refresh labels/history.
    Status(GuiStatus),
}

/// Handles the engine holds for GUI mode.
pub struct GuiHooks {
    pub actions: UnboundedReceiver<GuiAction>,
    pub events: Sender<GuiEvent>,
    pub paused: Arc<AtomicBool>,
}

impl GuiHooks {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// Entry point (main thread)
// ---------------------------------------------------------------------------

/// Run the GUI: engine on a worker thread, AppKit on the main thread.
/// Blocks inside `NSApplication.run()` until Quit.
pub fn run(config: nf_config::AppConfig) {
    let (action_tx, action_rx) = tokio::sync::mpsc::unbounded_channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let paused = Arc::new(AtomicBool::new(false));
    let engine_paused = paused.clone();

    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(crate::run_monitor(
                config,
                Some(GuiHooks {
                    actions: action_rx,
                    events: event_tx,
                    paused: engine_paused,
                }),
            ));
        })
        .expect("spawn engine thread");

    // Re-read defaults for initial field values (engine owns `config` now).
    let initial = nf_config::AppConfig::load();
    unsafe { main_thread_run(&initial, action_tx, event_rx) };
}

unsafe fn main_thread_run(
    initial: &nf_config::AppConfig,
    action_tx: UnboundedSender<GuiAction>,
    event_rx: Receiver<GuiEvent>,
) {
    let mtm = MainThreadMarker::new().expect("GUI must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    // Accessory = menu-bar agent, no Dock icon. (Prohibited = 2 hides ALL UI.)
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let controller = Controller::new(mtm, &app, action_tx, event_rx, initial);
    // Menu-item targets and NSApplication.delegate are weak; keep the
    // controller alive process-wide so Launch Services gets a real handshake.
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(controller.clone()));
    app.setDelegate(Some(ProtocolObject::from_ref(&*controller)));

    // Realtime refresh pump (runloop retains scheduled timers).
    let target: &AnyObject = any(&controller);
    let _timer = NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
        0.25,
        target,
        sel!(tick:),
        None,
        true,
    );

    app.run();
}

/// `&Controller` viewed as `&AnyObject` (sound: every instance is an NSObject).
fn any(obj: &Controller) -> &AnyObject {
    unsafe { &*std::ptr::from_ref::<Controller>(obj).cast::<AnyObject>() }
}

thread_local! {
    static CONTROLLER: RefCell<Option<Retained<Controller>>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------------------
// Controller (main-thread AppKit object)
// ---------------------------------------------------------------------------

const WIN_W: f64 = 500.0;
const WIN_H: f64 = 580.0;
const MARGIN: f64 = 20.0;
const CONTENT_W: f64 = WIN_W - 2.0 * MARGIN; // 460.0
const CONTENT_H: f64 = 510.0;
const FIELD_H: f64 = 26.0;
const MAX_LOG_LINES: usize = 400;

/// All live controls, built once in [`Controller::build_ui`].
struct UiRefs {
    window: Retained<NSWindow>,
    status_item: Retained<NSStatusItem>,
    tray_status_line: Retained<NSMenuItem>,
    tray_pause_item: Retained<NSMenuItem>,
    segmented_tabs: Retained<NSSegmentedControl>,
    activity_container: Retained<NSView>,
    webhook_container: Retained<NSView>,
    diagnostics_container: Retained<NSView>,
    status_badge: Retained<NSTextField>,
    status_line: Retained<NSTextField>,
    sub_line: Retained<NSTextField>,
    count_forwarded: Retained<NSTextField>,
    count_today: Retained<NSTextField>,
    count_duplicates: Retained<NSTextField>,
    count_filtered: Retained<NSTextField>,
    recent_view: Retained<NSTextView>,
    console_view: Retained<NSTextView>,
    webhook_field: Retained<NSTextField>,
    webhook_headers_view: Retained<NSTextView>,
    webhook_result: Retained<NSTextField>,
    mode_line: Retained<NSTextField>,
    allowed_field: Retained<NSTextField>,
    pause_button: Retained<NSButton>,
    open_settings_button: Retained<NSButton>,
    diag_settings_button: Retained<NSButton>,
}

struct ControllerState {
    last_status: Option<GuiStatus>,
    log: VecDeque<String>,
    ax_prompted: bool,
    launch_complete: bool,
}

struct ControllerIvars {
    mtm: MainThreadMarker,
    app: Retained<NSApplication>,
    actions: UnboundedSender<GuiAction>,
    events: Receiver<GuiEvent>,
    ui: RefCell<Option<UiRefs>>,
    state: RefCell<ControllerState>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ControllerIvars]
    struct Controller;

    impl Controller {
        #[unsafe(method(openWindow:))]
        fn open_window(&self, _sender: Option<&AnyObject>) {
            self.show_window();
        }

        #[unsafe(method(tabSelected:))]
        fn tab_selected(&self, _sender: Option<&AnyObject>) {
            let ui = self.ui();
            let index = ui.segmented_tabs.selectedSegment();
            ui.activity_container.setHidden(index != 0);
            ui.webhook_container.setHidden(index != 1);
            ui.diagnostics_container.setHidden(index != 2);
        }

        #[unsafe(method(pauseToggle:))]
        fn pause_toggle(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.send(GuiAction::TogglePause);
        }

        #[unsafe(method(testWebhook:))]
        fn test_webhook(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.send(GuiAction::TestWebhook);
        }

        #[unsafe(method(saveWebhook:))]
        fn save_webhook(&self, _sender: Option<&AnyObject>) {
            let ui = self.ui();
            let url = ui.webhook_field.stringValue().to_string();
            let raw_headers = ui.webhook_headers_view.string().to_string();
            let headers = match parse_header_lines(&raw_headers) {
                Ok(headers) => headers,
                Err(error) => {
                    ui.webhook_result
                        .setTextColor(Some(&NSColor::systemRedColor()));
                    ui.webhook_result
                        .setStringValue(&NSString::from_str(&error));
                    return;
                }
            };
            ui.webhook_result
                .setTextColor(Some(&NSColor::secondaryLabelColor()));
            ui.webhook_result.setStringValue(ns_string!("Saving…"));
            let _ = self
                .ivars()
                .actions
                .send(GuiAction::SetWebhookConfig { url, headers });
        }

        #[unsafe(method(useAllApps:))]
        fn use_all_apps(&self, _sender: Option<&AnyObject>) {
            self.send_filter(FilterMode::All);
        }

        #[unsafe(method(useSelectedApps:))]
        fn use_selected_apps(&self, _sender: Option<&AnyObject>) {
            self.send_filter(FilterMode::Selected);
        }

        #[unsafe(method(saveAllowedApps:))]
        fn save_allowed_apps(&self, _sender: Option<&AnyObject>) {
            let mode = self
                .ivars()
                .state
                .borrow()
                .last_status
                .as_ref()
                .map(|s| s.filter_mode)
                .unwrap_or(FilterMode::All);
            self.send_filter(mode);
        }

        #[unsafe(method(dumpTree:))]
        fn dump_tree(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.send(GuiAction::DumpTree);
        }

        #[unsafe(method(testDetection:))]
        fn test_detection(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.send(GuiAction::TestDetection);
        }

        #[unsafe(method(openSettings:))]
        fn open_settings(&self, _sender: Option<&AnyObject>) {
            let _ = nf_accessibility::prompt_accessibility();
            open_accessibility_settings();
            self.show_window();
        }

        #[unsafe(method(promptPermission:))]
        fn prompt_permission(&self, _timer: Option<&NSTimer>) {
            self.maybe_prompt_permission();
        }

        #[unsafe(method(quitApp:))]
        fn quit_app(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().actions.send(GuiAction::Quit);
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _timer: Option<&NSTimer>) {
            if !self.ivars().state.borrow().launch_complete {
                self.drain_events();
                return;
            }
            self.apply_permission_state(nf_accessibility::accessibility_trusted());
            self.drain_events();
        }
    }

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSApplicationDelegate for Controller {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn application_did_finish_launching(&self, _notification: &NSNotification) {
            self.ivars().state.borrow_mut().launch_complete = true;
            self.show_window();
            self.apply_permission_state(nf_accessibility::accessibility_trusted());
            // Defer the modal permission dialog until after Launch Services
            // has received this callback. A nested NSAlert during launch is
            // what produced `_LSOpenURLsWithCompletionHandler() … -1712`.
            let target: &AnyObject = any(self);
            let _ = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.5,
                    target,
                    sel!(promptPermission:),
                    None,
                    false,
                )
            };
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn application_should_terminate_after_last_window_closed(
            &self,
            _sender: &NSApplication,
        ) -> bool {
            false
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn application_should_handle_reopen(
            &self,
            _sender: &NSApplication,
            _has_visible_windows: bool,
        ) -> bool {
            self.show_window();
            true
        }
    }
);

impl Controller {
    fn new(
        mtm: MainThreadMarker,
        app: &NSApplication,
        actions: UnboundedSender<GuiAction>,
        events: Receiver<GuiEvent>,
        initial: &nf_config::AppConfig,
    ) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(ControllerIvars {
            mtm,
            app: app.retain(),
            actions,
            events,
            ui: RefCell::new(None),
            state: RefCell::new(ControllerState {
                last_status: None,
                log: VecDeque::new(),
                ax_prompted: false,
                launch_complete: false,
            }),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe { this.build_ui(initial) };
        this
    }

    fn ui(&self) -> Ref<'_, UiRefs> {
        Ref::map(self.ivars().ui.borrow(), |o: &Option<UiRefs>| {
            o.as_ref().expect("UI built before use")
        })
    }

    fn show_window(&self) {
        let window = self.ui().window.clone();
        #[allow(deprecated)]
        self.ivars().app.activateIgnoringOtherApps(true);
        window.makeKeyAndOrderFront(None);
    }

    fn send_filter(&self, mode: FilterMode) {
        let ui = self.ui();
        let allowed = ui.allowed_field.stringValue().to_string();
        let allowed = allowed
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let _ = self
            .ivars()
            .actions
            .send(GuiAction::SetFilter { mode, allowed });
    }

    fn maybe_prompt_permission(&self) {
        {
            let mut state = self.ivars().state.borrow_mut();
            if state.ax_prompted {
                return;
            }
            state.ax_prompted = true;
        }
        if nf_accessibility::accessibility_trusted() {
            return;
        }
        let _ = nf_accessibility::prompt_accessibility();
        self.show_permission_alert();
    }

    fn show_permission_alert(&self) {
        let mtm = self.ivars().mtm;
        let alert = NSAlert::new(mtm);
        alert.setAlertStyle(NSAlertStyle::Warning);
        alert.setMessageText(ns_string!("Accessibility Permission Required"));
        alert.setInformativeText(ns_string!(
            "Notification Forwarder needs Accessibility permission to read notification information from macOS Notification Center.\n\nSystem Settings → Privacy & Security → Accessibility → enable Notification Forwarder."
        ));
        alert.addButtonWithTitle(ns_string!("Open System Settings"));
        alert.addButtonWithTitle(ns_string!("Later"));
        let response = alert.runModal();
        if response == NSAlertFirstButtonReturn {
            open_accessibility_settings();
        }
        self.show_window();
    }

    fn apply_permission_state(&self, trusted: bool) {
        let ui = self.ui();
        ui.open_settings_button.setHidden(trusted);
        ui.diag_settings_button.setHidden(trusted);
        ui.pause_button.setHidden(!trusted);

        if !trusted {
            ui.status_badge
                .setTextColor(Some(&NSColor::systemRedColor()));
            ui.status_badge
                .setStringValue(ns_string!("⚠️ Permission Required"));
            ui.status_line.setStringValue(ns_string!(
                "Notification Forwarder needs Accessibility permission to observe notifications."
            ));
            ui.sub_line.setStringValue(ns_string!(
                "System Settings → Privacy & Security → Accessibility → enable NotificationForwarder"
            ));
            ui.tray_status_line
                .setTitle(ns_string!("⚠ Permission required"));
            if let Some(button) = ui.status_item.button(self.ivars().mtm) {
                button.setToolTip(Some(ns_string!(
                    "Notification Forwarder — Accessibility permission required"
                )));
            }
            return;
        }

        self.render_active_status();
    }

    fn render_active_status(&self) {
        let ui = self.ui();
        let state = self.ivars().state.borrow();
        let last = state.last_status.as_ref();

        let is_paused = last.map(|s| !s.monitoring).unwrap_or(false);
        let dot = if !is_paused { "●" } else { "○" };
        let word = if !is_paused {
            "Monitoring Active"
        } else {
            "Monitoring Paused"
        };

        let green = NSColor::systemGreenColor();
        let orange = NSColor::systemOrangeColor();
        ui.status_badge.setTextColor(Some(if !is_paused {
            &green
        } else {
            &orange
        }));
        ui.status_badge
            .setStringValue(&NSString::from_str(&format!("{dot} {word}")));
        ui.status_line.setStringValue(&NSString::from_str(if !is_paused {
            "Forwarding incoming notifications to webhook endpoint"
        } else {
            "Notification monitoring is temporarily paused"
        }));

        if let Some(status) = last {
            ui.sub_line.setStringValue(&NSString::from_str(&format!(
                "AX {} · NC {} · Webhook {}",
                status.accessibility, status.notification_center, status.webhook
            )));
            ui.count_forwarded
                .setStringValue(&NSString::from_str(&status.forwarded.to_string()));
            ui.count_today
                .setStringValue(&NSString::from_str(&status.today.to_string()));
            ui.count_duplicates
                .setStringValue(&NSString::from_str(&status.duplicates.to_string()));
            ui.count_filtered
                .setStringValue(&NSString::from_str(&status.filtered.to_string()));

            ui.mode_line.setStringValue(&NSString::from_str(&format!(
                "Mode: {}",
                match status.filter_mode {
                    FilterMode::All => "all applications".to_string(),
                    FilterMode::Selected => "selected only".to_string(),
                }
            )));
            ui.webhook_result
                .setTextColor(Some(&NSColor::secondaryLabelColor()));
            ui.webhook_result
                .setStringValue(&NSString::from_str(&status.webhook));

            let recent: String = if status.recent.is_empty() {
                "No notifications detected yet.\n\nWhen apps (e.g. WhatsApp, Messages) receive notifications, they will appear here.".to_string()
            } else {
                status
                    .recent
                    .iter()
                    .map(|r| {
                        if r.message.is_empty() {
                            format!("• {} — {}", r.app, r.title)
                        } else {
                            format!("• {} — {}\n  {}", r.app, r.title, r.message)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n")
            };
            ui.recent_view.setString(&NSString::from_str(&recent));

            ui.tray_status_line.setTitle(&NSString::from_str(&format!(
                "{dot} {} · {} today",
                if status.monitoring { "Monitoring" } else { "Paused" },
                status.today
            )));
        } else {
            ui.sub_line.setStringValue(ns_string!("AX ✓ Enabled · NC Connecting… · Webhook Ready"));
            ui.tray_status_line.setTitle(ns_string!("● Monitoring"));
        }

        ui.pause_button
            .setTitle(&NSString::from_str(if !is_paused {
                "Pause"
            } else {
                "Resume"
            }));
        ui.tray_pause_item
            .setTitle(&NSString::from_str(if !is_paused {
                "Pause Monitoring"
            } else {
                "Resume Monitoring"
            }));

        if let Some(button) = ui.status_item.button(self.ivars().mtm) {
            button.setToolTip(Some(&NSString::from_str(&format!(
                "Notification Forwarder — {word}"
            ))));
        }
    }

    // -- event pump ----------------------------------------------------------

    fn drain_events(&self) {
        let mut status_changed = false;
        let mut log_changed = false;
        while let Ok(event) = self.ivars().events.try_recv() {
            match event {
                GuiEvent::Log(line) => {
                    let mut state = self.ivars().state.borrow_mut();
                    state.log.push_back(line);
                    while state.log.len() > MAX_LOG_LINES {
                        state.log.pop_front();
                    }
                    log_changed = true;
                }
                GuiEvent::Status(status) => {
                    if status.exiting {
                        self.ivars().app.terminate(None);
                        return;
                    }
                    self.ivars().state.borrow_mut().last_status = Some(status);
                    status_changed = true;
                }
            }
        }
        if log_changed {
            let text: String = self
                .ivars()
                .state
                .borrow()
                .log
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let ui = self.ui();
            unsafe {
                ui.console_view.setString(&NSString::from_str(&text));
                let sender: Option<&AnyObject> = None;
                let _: () = msg_send![&ui.console_view, scrollToEndOfDocument: sender];
            }
        }
        if status_changed {
            self.refresh_status();
        }
    }

    fn refresh_status(&self) {
        let trusted = nf_accessibility::accessibility_trusted();
        self.apply_permission_state(trusted);
    }

    // -- UI construction ------------------------------------------------------

    unsafe fn build_ui(&self, initial: &nf_config::AppConfig) {
        let mtm = self.ivars().mtm;

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc::<NSWindow>(),
                rect(0.0, 0.0, WIN_W, WIN_H),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.center();
        window.setTitle(&NSString::from_str("Notification Forwarder"));
        window.setReleasedWhenClosed(false);
        window.setContentSize(NSSize::new(WIN_W, WIN_H));
        window.setMinSize(NSSize::new(WIN_W, WIN_H));
        window.setMaxSize(NSSize::new(WIN_W, WIN_H));
        if let Some(zoom_button) = window.standardWindowButton(NSWindowButton::ZoomButton) {
            zoom_button.setEnabled(false);
        }
        let content = window.contentView().expect("window content view");

        // Tray icon + menu
        let bar = NSStatusBar::systemStatusBar();
        let item = bar.statusItemWithLength(-2.0);
        item.setVisible(true);
        item.setAutosaveName(Some(ns_string!("NotificationForwarderStatusItem")));
        if let Some(button) = item.button(mtm) {
            if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("bell.fill"),
                Some(ns_string!("Notification Forwarder")),
            ) {
                image.setTemplate(true);
                button.setImage(Some(&image));
            } else {
                button.setTitle(ns_string!("NF"));
            }
            button.setToolTip(Some(ns_string!("Notification Forwarder")));
        }
        let menu = NSMenu::new(mtm);
        let target: &AnyObject = any(self);
        let tray_status_line = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("● Starting…"),
            None,
            ns_string!(""),
        );
        tray_status_line.setEnabled(false);
        let open = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Open"),
            Some(sel!(openWindow:)),
            ns_string!(""),
        );
        open.setTarget(Some(target));
        let tray_pause_item = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Pause Monitoring"),
            Some(sel!(pauseToggle:)),
            ns_string!(""),
        );
        tray_pause_item.setTarget(Some(target));
        let test = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Test Webhook"),
            Some(sel!(testWebhook:)),
            ns_string!(""),
        );
        test.setTarget(Some(target));
        let settings = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Open System Settings…"),
            Some(sel!(openSettings:)),
            ns_string!(""),
        );
        settings.setTarget(Some(target));
        let quit = menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Quit"),
            Some(sel!(quitApp:)),
            ns_string!(""),
        );
        quit.setTarget(Some(target));
        item.setMenu(Some(&menu));

        // Top Navigation: Segmented Control
        let tab_labels = NSArray::from_slice(&[
            ns_string!("Activity"),
            ns_string!("Webhook & Filters"),
            ns_string!("Diagnostics"),
        ]);
        let segmented_tabs = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &tab_labels,
                NSSegmentSwitchTracking::SelectOne,
                Some(target),
                Some(sel!(tabSelected:)),
                mtm,
            )
        };
        segmented_tabs.setFrame(rect(MARGIN, WIN_H - 44.0, CONTENT_W, 28.0));
        segmented_tabs.setSelectedSegment(0);
        content.addSubview(&segmented_tabs);

        // Container views for each tab
        let activity_container =
            NSView::initWithFrame(mtm.alloc::<NSView>(), rect(MARGIN, 14.0, CONTENT_W, CONTENT_H));
        let webhook_container =
            NSView::initWithFrame(mtm.alloc::<NSView>(), rect(MARGIN, 14.0, CONTENT_W, CONTENT_H));
        let diagnostics_container =
            NSView::initWithFrame(mtm.alloc::<NSView>(), rect(MARGIN, 14.0, CONTENT_W, CONTENT_H));
        activity_container.setHidden(false);
        webhook_container.setHidden(true);
        diagnostics_container.setHidden(true);
        content.addSubview(&activity_container);
        content.addSubview(&webhook_container);
        content.addSubview(&diagnostics_container);

        // ==========================================
        // 1. ACTIVITY TAB
        // ==========================================
        // Status Hero Card
        let status_card = add_card_box(mtm, &activity_container, 0.0, 382.0, CONTENT_W, 128.0);
        let status_badge = add_label(mtm, &status_card, "● Starting…", 16.0, 92.0, 240.0);
        status_badge.setFont(Some(&NSFont::boldSystemFontOfSize(15.0)));
        status_badge.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let pause_button = add_button(
            mtm,
            &status_card,
            "Pause",
            sel!(pauseToggle:),
            target,
            348.0,
            86.0,
            96.0,
        );
        let open_settings_button = add_button(
            mtm,
            &status_card,
            "Open System Settings",
            sel!(openSettings:),
            target,
            264.0,
            86.0,
            180.0,
        );
        open_settings_button.setHidden(true);

        let status_line = add_label(
            mtm,
            &status_card,
            "Forwarding incoming notifications to webhook endpoint",
            16.0,
            60.0,
            428.0,
        );
        status_line.setFont(Some(&NSFont::systemFontOfSize(13.0)));

        let sub_line = add_label(
            mtm,
            &status_card,
            "AX … · NC … · Webhook …",
            16.0,
            24.0,
            428.0,
        );
        sub_line.setTextColor(Some(&NSColor::secondaryLabelColor()));
        sub_line.setFont(Some(&NSFont::systemFontOfSize(11.0)));

        // Metrics Row Card (4 columns)
        let metrics_card = add_card_box(mtm, &activity_container, 0.0, 294.0, CONTENT_W, 76.0);
        let col_w = 105.0;

        // Forwarded
        let count_forwarded = add_label(mtm, &metrics_card, "0", 12.0, 36.0, col_w);
        count_forwarded.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
        let lbl_forwarded = add_label(mtm, &metrics_card, "FORWARDED", 12.0, 14.0, col_w);
        lbl_forwarded.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
        lbl_forwarded.setTextColor(Some(&NSColor::secondaryLabelColor()));

        // Today
        let count_today = add_label(mtm, &metrics_card, "0", 124.0, 36.0, col_w);
        count_today.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
        let lbl_today = add_label(mtm, &metrics_card, "TOTAL TODAY", 124.0, 14.0, col_w);
        lbl_today.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
        lbl_today.setTextColor(Some(&NSColor::secondaryLabelColor()));

        // Duplicates
        let count_duplicates = add_label(mtm, &metrics_card, "0", 236.0, 36.0, col_w);
        count_duplicates.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
        let lbl_duplicates = add_label(mtm, &metrics_card, "DUPLICATES", 236.0, 14.0, col_w);
        lbl_duplicates.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
        lbl_duplicates.setTextColor(Some(&NSColor::secondaryLabelColor()));

        // Filtered
        let count_filtered = add_label(mtm, &metrics_card, "0", 348.0, 36.0, col_w);
        count_filtered.setFont(Some(&NSFont::boldSystemFontOfSize(20.0)));
        let lbl_filtered = add_label(mtm, &metrics_card, "FILTERED", 348.0, 14.0, col_w);
        lbl_filtered.setFont(Some(&NSFont::boldSystemFontOfSize(10.0)));
        lbl_filtered.setTextColor(Some(&NSColor::secondaryLabelColor()));

        // Recent Notifications Card
        add_section_label(
            mtm,
            &activity_container,
            "RECENT NOTIFICATIONS",
            4.0,
            262.0,
            CONTENT_W,
        );
        let recent_card = add_card_box(mtm, &activity_container, 0.0, 0.0, CONTENT_W, 256.0);
        let (_, recent_view) = add_scroll_text(
            mtm,
            &recent_card,
            6.0,
            6.0,
            CONTENT_W - 12.0,
            244.0,
            false,
            false,
        );
        recent_view.setString(ns_string!(
            "No notifications detected yet.\n\nWhen apps (e.g. WhatsApp, Messages) receive notifications, they will appear here."
        ));

        // ==========================================
        // 2. WEBHOOK & FILTERS TAB
        // ==========================================
        // Webhook Destination Card
        let webhook_card = add_card_box(mtm, &webhook_container, 0.0, 350.0, CONTENT_W, 160.0);
        add_section_label(mtm, &webhook_card, "WEBHOOK DESTINATION", 16.0, 130.0, 428.0);
        add_secondary_label(mtm, &webhook_card, "Destination URL", 16.0, 106.0, 428.0);
        let webhook_field = add_field(
            mtm,
            &webhook_card,
            &initial.webhook_url,
            "https://your-server.com/api/webhook",
            16.0,
            72.0,
            428.0,
        );
        add_button(
            mtm,
            &webhook_card,
            "Save",
            sel!(saveWebhook:),
            target,
            16.0,
            30.0,
            80.0,
        );
        add_button(
            mtm,
            &webhook_card,
            "Test Webhook",
            sel!(testWebhook:),
            target,
            102.0,
            30.0,
            120.0,
        );
        let webhook_result = add_label(mtm, &webhook_card, "", 230.0, 34.0, 214.0);
        webhook_result.setTextColor(Some(&NSColor::secondaryLabelColor()));
        webhook_result.setFont(Some(&NSFont::systemFontOfSize(11.0)));

        // Custom Headers Card
        let headers_card = add_card_box(mtm, &webhook_container, 0.0, 180.0, CONTENT_W, 158.0);
        add_section_label(mtm, &headers_card, "CUSTOM HTTP HEADERS", 16.0, 128.0, 428.0);
        add_secondary_label(
            mtm,
            &headers_card,
            "One per line: Header-Name: value",
            16.0,
            106.0,
            428.0,
        );
        let (_, webhook_headers_view) = add_scroll_text(
            mtm,
            &headers_card,
            16.0,
            16.0,
            428.0,
            84.0,
            true,
            true,
        );
        webhook_headers_view.setString(&NSString::from_str(
            &initial
                .webhook_headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));

        // Application Filter Card
        let filter_card = add_card_box(mtm, &webhook_container, 0.0, 0.0, CONTENT_W, 168.0);
        add_section_label(mtm, &filter_card, "APPLICATION FILTERING", 16.0, 134.0, 220.0);
        let mode_line = add_secondary_label(mtm, &filter_card, "Mode: …", 16.0, 110.0, 200.0);
        add_button(
            mtm,
            &filter_card,
            "All",
            sel!(useAllApps:),
            target,
            270.0,
            104.0,
            76.0,
        );
        add_button(
            mtm,
            &filter_card,
            "Selected",
            sel!(useSelectedApps:),
            target,
            352.0,
            104.0,
            92.0,
        );
        add_secondary_label(
            mtm,
            &filter_card,
            "Allowed app names (comma-separated):",
            16.0,
            80.0,
            428.0,
        );
        let allowed_field = add_field(
            mtm,
            &filter_card,
            &initial
                .allowed_apps
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            "WhatsApp, Messages, Slack",
            16.0,
            46.0,
            330.0,
        );
        add_button(
            mtm,
            &filter_card,
            "Save Apps",
            sel!(saveAllowedApps:),
            target,
            352.0,
            45.0,
            92.0,
        );
        let filter_hint = add_secondary_label(
            mtm,
            &filter_card,
            "Leave blank or select All to forward from any application.",
            16.0,
            18.0,
            428.0,
        );
        filter_hint.setFont(Some(&NSFont::systemFontOfSize(10.0)));

        // ==========================================
        // 3. DIAGNOSTICS TAB
        // ==========================================
        let diag_card = add_card_box(mtm, &diagnostics_container, 0.0, 436.0, CONTENT_W, 74.0);
        add_button(
            mtm,
            &diag_card,
            "Test Detection",
            sel!(testDetection:),
            target,
            16.0,
            22.0,
            132.0,
        );
        add_button(
            mtm,
            &diag_card,
            "Dump AX Tree",
            sel!(dumpTree:),
            target,
            156.0,
            22.0,
            132.0,
        );
        let diag_settings_button = add_button(
            mtm,
            &diag_card,
            "Open Settings",
            sel!(openSettings:),
            target,
            296.0,
            22.0,
            148.0,
        );

        add_section_label(
            mtm,
            &diagnostics_container,
            "REALTIME ACTIVITY CONSOLE",
            4.0,
            400.0,
            CONTENT_W,
        );
        let console_card = add_card_box(mtm, &diagnostics_container, 0.0, 0.0, CONTENT_W, 394.0);
        let (_, console_view) = add_scroll_text(
            mtm,
            &console_card,
            6.0,
            6.0,
            CONTENT_W - 12.0,
            382.0,
            false,
            true,
        );

        *self.ivars().ui.borrow_mut() = Some(UiRefs {
            window,
            status_item: item,
            tray_status_line,
            tray_pause_item,
            segmented_tabs,
            activity_container,
            webhook_container,
            diagnostics_container,
            status_badge,
            status_line,
            sub_line,
            count_forwarded,
            count_today,
            count_duplicates,
            count_filtered,
            recent_view,
            console_view,
            webhook_field,
            webhook_headers_view,
            webhook_result,
            mode_line,
            allowed_field,
            pause_button,
            open_settings_button,
            diag_settings_button,
        });
    }
}

fn parse_header_lines(input: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers = Vec::new();
    for (index, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!(
                "Line {} needs a colon between name and value.",
                index + 1
            ));
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            return Err(format!("Line {} has an empty header name.", index + 1));
        }
        headers.push((name.to_string(), value.to_string()));
    }
    Ok(headers)
}

fn open_accessibility_settings() {
    // Open System Settings.app by bundle id. The `x-apple.systempreferences:`
    // pane URLs time out with LS error -1712 on recent macOS.
    let workspace = NSWorkspace::sharedWorkspace();
    if let Some(app_url) =
        workspace.URLForApplicationWithBundleIdentifier(ns_string!("com.apple.systempreferences"))
    {
        let _ = workspace.openURL(&app_url);
    }
    // Best-effort jump to the Accessibility pane; ignore LS timeouts.
    let _ = std::process::Command::new("/usr/bin/open")
        .args([
            "-g",
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

// ---------------------------------------------------------------------------
// Control builders (absolute frames; content-view origin is bottom-left)
// ---------------------------------------------------------------------------

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn add_card_box(
    mtm: MainThreadMarker,
    parent: &NSView,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Retained<NSBox> {
    let card = NSBox::initWithFrame(mtm.alloc::<NSBox>(), rect(x, y, w, h));
    card.setBoxType(NSBoxType::Custom);
    card.setCornerRadius(10.0);
    card.setBorderWidth(1.0);
    card.setBorderColor(&NSColor::separatorColor());
    card.setFillColor(&NSColor::controlBackgroundColor());
    card.setContentViewMargins(NSSize::new(0.0, 0.0));
    parent.addSubview(&card);
    card
}

fn add_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFrame(rect(x, y, w, 22.0));
    parent.addSubview(&label);
    label
}

fn add_secondary_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSTextField> {
    let label = add_label(mtm, parent, text, x, y, w);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    label
}

fn add_section_label(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSTextField> {
    let label = add_label(mtm, parent, text, x, y, w);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));
    label.setFont(Some(&NSFont::boldSystemFontOfSize(11.0)));
    label
}

fn add_field(
    mtm: MainThreadMarker,
    parent: &NSView,
    text: &str,
    placeholder: &str,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSTextField> {
    let field = NSTextField::textFieldWithString(&NSString::from_str(text), mtm);
    field.setFrame(rect(x, y, w, FIELD_H));
    if !placeholder.is_empty() {
        field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    }
    parent.addSubview(&field);
    field
}

fn add_button(
    mtm: MainThreadMarker,
    parent: &NSView,
    title: &str,
    action: Sel,
    target: &AnyObject,
    x: f64,
    y: f64,
    w: f64,
) -> Retained<NSButton> {
    let button = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    };
    button.setFrame(rect(x, y, w, 28.0));
    parent.addSubview(&button);
    button
}

fn add_scroll_text(
    mtm: MainThreadMarker,
    parent: &NSView,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    editable: bool,
    monospaced: bool,
) -> (Retained<NSScrollView>, Retained<NSTextView>) {
    let scroll = NSScrollView::initWithFrame(mtm.alloc::<NSScrollView>(), rect(x, y, w, h));
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll.setDrawsBackground(false);

    let text = NSTextView::initWithFrame(mtm.alloc::<NSTextView>(), rect(0.0, 0.0, w, h));
    text.setEditable(editable);
    text.setSelectable(true);
    text.setDrawsBackground(false);
    text.setTextColor(Some(&NSColor::labelColor()));
    text.setTextContainerInset(NSSize::new(8.0, 8.0));
    if monospaced {
        if let Some(font) = NSFont::userFixedPitchFontOfSize(11.0) {
            text.setFont(Some(&font));
        } else {
            text.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        }
    } else {
        text.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    }
    scroll.setDocumentView(Some(text.as_ref()));
    parent.addSubview(scroll.as_ref());
    (scroll, text)
}

#[cfg(test)]
mod tests {
    use super::parse_header_lines;

    #[test]
    fn parses_header_editor_lines() {
        let headers = parse_header_lines(
            "Authorization: Bearer secret\nX-Source: Notification Forwarder\n\n",
        )
        .unwrap();
        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0, "Authorization");
        assert_eq!(headers[0].1, "Bearer secret");
    }

    #[test]
    fn rejects_header_line_without_colon() {
        let error = parse_header_lines("Authorization Bearer secret").unwrap_err();
        assert!(error.contains("Line 1"));
    }
}
