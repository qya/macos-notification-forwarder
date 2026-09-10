//! macOS Notification Forwarder — application entry point.
//!
//! Lifecycle ():
//! ```text
//! launch → load config → check Accessibility → find NC → AXObserver → monitoring
//! ```
//! Pipeline ():
//! ```text
//! AX event → scan → parse → filter → dedup → queue → POST (+retry)
//! ```
//!
//! Two modes: native GUI by default (menu-bar tray + status window, see
//! [`gui`]), headless monitor with `--no-gui`.

mod gui;
mod ui;

use std::collections::VecDeque;
use std::time::Duration;

use nf_accessibility::{
    accessibility_trusted, find_notification_center_pid, MonitorEvent, ObserverEngine,
};
use nf_config::{sanitize_url_for_logging, AppConfig};
use nf_notification::{AppFilter, DedupCache, Notification};
use nf_webhook::{validate_headers, WebhookClient, WebhookPayload, WebhookQueue};
use tracing::{error, info, warn};

use gui::{GuiAction, GuiEvent, GuiHooks, GuiStatus, RecentItem};
use ui::StatusSnapshot;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Default)]
struct Args {
    config: Option<String>,
    webhook_url: Option<String>,
    test_webhook: bool,
    dump_tree: bool,
    test_detection: bool,
    once: bool,
    status: bool,
    menu: bool,
    no_gui: bool,
    verbose: bool,
    help: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let mut args = Args::default();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--config" => {
                i += 1;
                args.config = argv.get(i).cloned();
            }
            s if s.starts_with("--config=") => {
                args.config = Some(s["--config=".len()..].to_string());
            }
            "--webhook-url" => {
                i += 1;
                args.webhook_url = argv.get(i).cloned();
            }
            s if s.starts_with("--webhook-url=") => {
                args.webhook_url = Some(s["--webhook-url=".len()..].to_string());
            }
            "--test-webhook" => args.test_webhook = true,
            "--dump-tree" => args.dump_tree = true,
            "--test-detection" => args.test_detection = true,
            "--once" => args.once = true,
            "--status" => args.status = true,
            "--menu" => args.menu = true,
            "--no-gui" => args.no_gui = true,
            "--verbose" | "-v" => args.verbose = true,
            "--help" | "-h" => args.help = true,
            _ => {}
        }
        i += 1;
    }
    args
}

fn print_help() {
    println!(
        "\
notification-forwarder {VERSION}

USAGE:
    notification-forwarder [OPTIONS]

    Default: native GUI (menu-bar tray + status window with realtime
    console, webhook settings, app filters, diagnostics).

OPTIONS:
    --no-gui              Headless monitor loop (log to stderr, no window)
    --config PATH         Config file path (default: ~/Library/Application Support/…)
    --webhook-url URL     Override webhook destination for this run
    --test-webhook        POST a test payload and exit
    --test-detection      Single AX scan, print parsed notifications, exit
    --dump-tree           Print the Notification Center AX tree, exit
    --once                Scan once and forward, then exit (no observer loop)
    --status              Print diagnostics and exit
    --menu                With --status: also print the menu-bar text
    -v, --verbose         Debug logging (or RUST_LOG=debug)
    -h, --help            This help

ENV:
    RUST_LOG              tracing filter (e.g. RUST_LOG=debug)
    NOTIFORWARDER_CONFIG  Config path override
"
    );
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(future)
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    let args = parse_args(&argv);

    if args.help {
        print_help();
        return;
    }

    if let Some(path) = &args.config {
        // SAFETY: single-threaded startup; no concurrent readers yet.
        unsafe { std::env::set_var("NOTIFORWARDER_CONFIG", path) };
    }

    let mut config = AppConfig::load();
    if let Some(url) = &args.webhook_url {
        config.webhook_url = url.clone();
    }

    init_logging(&config, args.verbose);
    info!("application started version={VERSION}");

    if config.diagnostic_logging {
        info!("diagnostic logging enabled");
    }

    // ── One-shot diagnostics ──────────────────────────────────────────
    if args.dump_tree {
        if let Err(e) = dump_tree_cmd() {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.test_detection {
        if let Err(e) = test_detection_cmd(&config) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.status {
        if let Err(e) = block_on(status_cmd(&config, args.menu)) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if args.test_webhook {
        if let Err(e) = block_on(test_webhook_cmd(&config)) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    // ── Accessibility gate (, AT-01/AT-07) ──────────────────────
    // Headless/one-shot: print instructions and exit. GUI mode always
    // launches so the menu-bar icon and permission screen can appear.
    if !accessibility_trusted() {
        warn!("accessibility permission not granted");
        if args.no_gui || args.once {
            let _ = nf_accessibility::prompt_accessibility();
            print_permission_screen();
            return;
        }
    } else {
        info!("accessibility permission: granted");
    }

    if args.once {
        if let Err(e) = block_on(run_once(&config)) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }

    if args.no_gui {
        block_on(run_monitor(config, None));
    } else {
        gui::run(config);
    }
}

fn init_logging(config: &AppConfig, verbose: bool) {
    let default_level = if verbose || config.diagnostic_logging {
        "debug"
    } else {
        "info"
    };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| {
        format!("notification_forwarder={default_level},nf_accessibility={default_level},nf_webhook={default_level}")
    });
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn print_permission_screen() {
    eprintln!(
        "\
Accessibility Permission Required

Notification Forwarder needs Accessibility permission to read
notification information from macOS Notification Center.

    [ Open System Settings ]

        System Settings → Privacy & Security → Accessibility
        → enable your terminal / notification-forwarder

Status:
● Permission not granted

The application will never attempt to circumvent the permission system.
"
    );
}

// ── Monitor mode ─────────────────────────────────────────────────────

#[derive(Default)]
struct Counts {
    today: u64,
    forwarded: u64,
    duplicates: u64,
    filtered: u64,
}

/// Mutable engine state; config can change live from the GUI.
struct Engine {
    config: AppConfig,
    filter: AppFilter,
    client: WebhookClient,
    queue: WebhookQueue,
    dedup: DedupCache,
    counts: Counts,
    recent: VecDeque<Notification>,
}

impl Engine {
    fn new(config: AppConfig) -> Self {
        let filter = AppFilter::from_config(&config);
        let client = WebhookClient::new(
            config.webhook_url.clone(),
            config.max_attempts,
            &config.webhook_headers,
        );
        let queue = WebhookQueue::spawn(client.clone(), 256);
        let dedup = DedupCache::new(
            Duration::from_secs(config.dedup_ttl_secs),
            config.dedup_capacity,
        );
        Self {
            config,
            filter,
            client,
            queue,
            dedup,
            counts: Counts::default(),
            recent: VecDeque::with_capacity(20),
        }
    }

    /// Rebuild filter/client/queue after a live config change.
    fn rebuild(&mut self) {
        self.filter = AppFilter::from_config(&self.config);
        self.client = WebhookClient::new(
            self.config.webhook_url.clone(),
            self.config.max_attempts,
            &self.config.webhook_headers,
        );
        self.queue = WebhookQueue::spawn(self.client.clone(), 256);
    }

    fn push_recent(&mut self, n: Notification) {
        if self.recent.len() >= 20 {
            self.recent.pop_front();
        }
        self.recent.push_back(n);
    }

    fn gui_log(hooks: &Option<GuiHooks>, line: String) {
        if let Some(hooks) = hooks {
            let _ = hooks.events.send(GuiEvent::Log(line));
        }
    }

    fn push_status(&self, hooks: &Option<GuiHooks>, exiting: bool) {
        let Some(hooks) = hooks else { return };
        let trusted = accessibility_trusted();
        let _ = hooks.events.send(GuiEvent::Status(GuiStatus {
            monitoring: trusted && !hooks.is_paused(),
            today: self.counts.today,
            forwarded: self.counts.forwarded,
            duplicates: self.counts.duplicates,
            filtered: self.counts.filtered,
            accessibility: if trusted {
                "✓ Enabled".to_string()
            } else {
                "✗ Permission required".to_string()
            },
            needs_permission: !trusted,
            notification_center: match find_notification_center_pid() {
                Some(pid) => format!("✓ Connected (pid {pid})"),
                None => "✗ Not found".to_string(),
            },
            webhook: format!(
                "Ready · {} custom header{}",
                self.config.webhook_headers.len(),
                if self.config.webhook_headers.len() == 1 {
                    ""
                } else {
                    "s"
                }
            ),
            recent: self
                .recent
                .iter()
                .rev()
                .take(8)
                .map(|n| RecentItem {
                    app: n.app_name.clone(),
                    title: n.title.clone(),
                    message: n.message.clone(),
                })
                .collect(),
            filter_mode: self.config.filter_mode,
            exiting,
        }));
    }
}

async fn recv_gui_action(hooks: &mut Option<GuiHooks>) -> Option<GuiAction> {
    match hooks {
        Some(h) => h.actions.recv().await,
        None => std::future::pending().await,
    }
}

/// Block until Accessibility is granted. Returns true if the user asked to quit.
async fn wait_until_trusted(engine: &mut Engine, hooks: &mut Option<GuiHooks>) -> bool {
    if accessibility_trusted() {
        return false;
    }
    Engine::gui_log(
        hooks,
        "⚠️ Accessibility permission required. System Settings → Privacy & Security → Accessibility."
            .to_string(),
    );
    engine.push_status(hooks, false);
    loop {
        if accessibility_trusted() {
            info!("accessibility permission: granted");
            Engine::gui_log(hooks, "✓ Accessibility permission granted.".to_string());
            engine.push_status(hooks, false);
            return false;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(750)) => {
                engine.push_status(hooks, false);
            }
            action = recv_gui_action(hooks) => {
                let Some(action) = action else { return true };
                if handle_action(engine, hooks, action).await {
                    return true;
                }
            }
        }
    }
}

async fn run_monitor(config: AppConfig, mut hooks: Option<GuiHooks>) {
    let pid = find_notification_center_pid();
    match pid {
        Some(pid) => info!(pid, "notification center pid"),
        None => warn!("notification Center not found at startup; observer will wait for it"),
    }

    let mut engine = Engine::new(config);
    let forwarding = engine.config.forwarding_enabled;
    if !forwarding {
        warn!("forwarding disabled in settings; detecting only");
    }
    engine.push_status(&hooks, false);

    loop {
        if wait_until_trusted(&mut engine, &mut hooks).await {
            break;
        }

        Engine::gui_log(&hooks, "Monitoring started.".to_string());
        engine.push_status(&hooks, false);

        let engine_observer = ObserverEngine::new(Duration::from_millis(engine.config.debounce_ms));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<MonitorEvent>(64);
        let observer_task = tokio::spawn(async move {
            engine_observer.run(tx).await;
        });

        info!("monitoring; send a notification to your Mac");
        let quit = loop {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else { break false };
                    match event {
                        MonitorEvent::PermissionRevoked => {
                            error!("accessibility permission revoked (AT-07)");
                            Engine::gui_log(&hooks, "⚠️ Accessibility permission revoked.".to_string());
                            if hooks.is_none() {
                                print_permission_screen();
                                break true;
                            }
                            break false;
                        }
                        MonitorEvent::Reconnected { .. } => {
                            info!("notification Center reconnected; monitoring resumed");
                            Engine::gui_log(&hooks, "↻ Notification Center reconnected.".to_string());
                            engine.push_status(&hooks, false);
                        }
                        MonitorEvent::Banners(banners) => {
                            if hooks.as_ref().is_some_and(|h| h.is_paused()) {
                                continue;
                            }
                            let mut changed = false;
                            for banner in banners {
                                let fields = banner.into_fields();
                                let ax_id = fields.identifier.clone();
                                let Some(notification) =
                                    nf_notification::parse_banner_fields(&fields)
                                else {
                                    continue;
                                };
                                engine.counts.today += 1;
                                changed = true;

                                if engine.config.diagnostic_logging {
                                    info!(
                                        app = %notification.app_name,
                                        title = %notification.title,
                                        ax_id = ?ax_id,
                                        "notification detected"
                                    );
                                } else {
                                    // Privacy (): bodies stay out of normal logs.
                                    info!(app = %notification.app_name, "notification detected");
                                }
                                Engine::gui_log(
                                    &hooks,
                                    format!("🔔 {} — {}", notification.app_name, notification.title),
                                );

                                if !engine.filter.allows(&notification.app_name) {
                                    engine.counts.filtered += 1;
                                    Engine::gui_log(
                                        &hooks,
                                        format!("  filtered (app not allowed): {}", notification.app_name),
                                    );
                                    continue;
                                }
                                let fingerprint = notification.fingerprint();
                                if engine.dedup.is_duplicate(&fingerprint) {
                                    engine.counts.duplicates += 1;
                                    Engine::gui_log(&hooks, "  duplicate ignored".to_string());
                                    continue;
                                }
                                engine.counts.forwarded += 1;
                                engine.push_recent(notification.clone());

                                if !forwarding {
                                    continue;
                                }
                                engine
                                    .queue
                                    .enqueue(WebhookPayload {
                                        app_name: notification.app_name.clone(),
                                        title: notification.title.clone(),
                                        message: notification.message.clone(),
                                        notification_id: notification.id.clone(),
                                        timestamp: notification.timestamp.to_rfc3339(),
                                    })
                                    .await;
                            }
                            if changed {
                                engine.push_status(&hooks, false);
                            }
                        }
                    }
                }
                action = recv_gui_action(&mut hooks) => {
                    let Some(action) = action else { break true };
                    if handle_action(&mut engine, &hooks, action).await {
                        break true;
                    }
                }
            }
        };
        observer_task.abort();
        if quit {
            break;
        }
    }
    engine.push_status(&hooks, true);
    let stats = engine.queue.stats().await;
    info!(
        today = engine.counts.today,
        forwarded = engine.counts.forwarded,
        duplicates = engine.counts.duplicates,
        filtered = engine.counts.filtered,
        delivered = stats.delivered,
        failed = stats.failed,
        "monitor loop ended"
    );
}

/// Handle one GUI action. Returns true when the engine should exit.
async fn handle_action(engine: &mut Engine, hooks: &Option<GuiHooks>, action: GuiAction) -> bool {
    match action {
        GuiAction::TogglePause => {
            if let Some(h) = hooks {
                let paused = !h.is_paused();
                h.paused.store(paused, std::sync::atomic::Ordering::Relaxed);
            }
            Engine::gui_log(
                hooks,
                if hooks.as_ref().is_some_and(|h| h.is_paused()) {
                    "○ Monitoring paused.".to_string()
                } else {
                    "● Monitoring resumed.".to_string()
                },
            );
            engine.push_status(hooks, false);
            false
        }
        GuiAction::TestWebhook => {
            Engine::gui_log(hooks, "Testing webhook…".to_string());
            match engine.client.send(&WebhookPayload::test()).await {
                Ok(status) => {
                    let line = format!("✅ Webhook OK (status {status})");
                    info!("{line}");
                    Engine::gui_log(hooks, line);
                }
                Err(e) => {
                    let line = format!("❌ Webhook failed: {e}");
                    warn!("{line}");
                    Engine::gui_log(hooks, line);
                }
            }
            engine.push_status(hooks, false);
            false
        }
        GuiAction::SetWebhookConfig { url, headers } => {
            let url = url.trim().to_string();
            if url.is_empty() {
                Engine::gui_log(hooks, "❌ Webhook URL is empty; not saved.".to_string());
            } else {
                let headers = headers.into_iter().collect();
                if let Err(error) = validate_headers(&headers) {
                    Engine::gui_log(hooks, format!("❌ Headers not saved: {error}"));
                    engine.push_status(hooks, false);
                    return false;
                }
                engine.config.webhook_url = url;
                engine.config.webhook_headers = headers;
                match engine.config.save() {
                    Ok(()) => Engine::gui_log(
                        hooks,
                        format!(
                            "Webhook settings saved ({} custom header{}).",
                            engine.config.webhook_headers.len(),
                            if engine.config.webhook_headers.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ),
                    ),
                    Err(e) => Engine::gui_log(hooks, format!("❌ Save failed: {e}")),
                }
                engine.rebuild();
            }
            engine.push_status(hooks, false);
            false
        }
        GuiAction::SetFilter { mode, allowed } => {
            engine.config.filter_mode = mode;
            engine.config.allowed_apps = allowed.into_iter().collect();
            match engine.config.save() {
                Ok(()) => Engine::gui_log(hooks, "Application filter saved.".to_string()),
                Err(e) => Engine::gui_log(hooks, format!("❌ Save failed: {e}")),
            }
            engine.rebuild();
            engine.push_status(hooks, false);
            false
        }
        GuiAction::DumpTree => {
            for line in engine_dump_tree() {
                Engine::gui_log(hooks, line);
            }
            false
        }
        GuiAction::TestDetection => {
            for line in engine_test_detection(engine) {
                Engine::gui_log(hooks, line);
            }
            false
        }
        GuiAction::Quit => true,
    }
}

fn engine_dump_tree() -> Vec<String> {
    if !accessibility_trusted() {
        return vec!["❌ Accessibility permission not granted.".to_string()];
    }
    let Ok(pid) = find_notification_center_pid().ok_or("Notification Center process not found")
    else {
        return vec!["❌ Notification Center process not found.".to_string()];
    };
    let Some(app) = axuielement::AXUIElement::from_pid(pid) else {
        return vec!["❌ Failed to create AXUIElement.".to_string()];
    };
    std::iter::once(format!("AX tree (pid {pid}):"))
        .chain(
            nf_accessibility::dump_tree(&app, 10)
                .lines()
                .map(str::to_string),
        )
        .collect()
}

fn engine_test_detection(engine: &Engine) -> Vec<String> {
    match nf_accessibility::scan_once() {
        Err(e) => vec![format!("❌ Scan failed: {e}")],
        Ok(banners) if banners.is_empty() => vec![
            "No banners visible. Send a notification, keep it on screen, and retry.".to_string(),
        ],
        Ok(banners) => banners
            .into_iter()
            .map(
                |banner| match nf_notification::parse_banner_fields(&banner.into_fields()) {
                    Some(n) => {
                        let verdict = if engine.filter.allows(&n.app_name) {
                            "would forward"
                        } else {
                            "would filter"
                        };
                        format!("🔔 {} — {} [{verdict}]", n.app_name, n.title)
                    }
                    None => "(empty banner ignored)".to_string(),
                },
            )
            .collect(),
    }
}

// ── One-shot commands ────────────────────────────────────────────────

/// Scan once and forward (used by `--once` and launchd testing).
async fn run_once(config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    if !accessibility_trusted() {
        print_permission_screen();
        return Ok(());
    }
    let banners = nf_accessibility::scan_once().map_err(|e| format!("scan failed: {e}"))?;
    info!(count = banners.len(), "one-shot scan complete");
    let filter = AppFilter::from_config(config);
    let client = WebhookClient::new(
        config.webhook_url.clone(),
        config.max_attempts,
        &config.webhook_headers,
    );
    for banner in banners {
        let Some(n) = nf_notification::parse_banner_fields(&banner.into_fields()) else {
            continue;
        };
        println!("🔔 {} — {}: {}", n.app_name, n.title, n.message);
        if !filter.allows(&n.app_name) {
            println!("   (filtered)");
            continue;
        }
        match client
            .send(&WebhookPayload {
                app_name: n.app_name.clone(),
                title: n.title.clone(),
                message: n.message.clone(),
                notification_id: n.id.clone(),
                timestamp: n.timestamp.to_rfc3339(),
            })
            .await
        {
            Ok(status) => println!("   → webhook {status}"),
            Err(e) => eprintln!("   → webhook failed: {e}"),
        }
    }
    Ok(())
}

fn dump_tree_cmd() -> Result<(), Box<dyn std::error::Error>> {
    for line in engine_dump_tree() {
        println!("{line}");
    }
    Ok(())
}

fn test_detection_cmd(config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    if !accessibility_trusted() {
        print_permission_screen();
        return Ok(());
    }
    let engine = Engine::new(config.clone());
    for line in engine_test_detection(&engine) {
        println!("{line}");
    }
    Ok(())
}

async fn test_webhook_cmd(config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let client = WebhookClient::new(
        config.webhook_url.clone(),
        config.max_attempts,
        &config.webhook_headers,
    );
    println!("POST {}", sanitize_url_for_logging(&config.webhook_url));
    match client.send(&WebhookPayload::test()).await {
        Ok(status) => {
            println!("✅ Webhook connected (status {status})");
            Ok(())
        }
        Err(e) => {
            eprintln!("❌ Webhook failed: {e}");
            Err(e.to_string().into())
        }
    }
}

async fn status_cmd(config: &AppConfig, menu: bool) -> Result<(), Box<dyn std::error::Error>> {
    let accessible = accessibility_trusted();
    let nc = find_notification_center_pid();
    let snapshot = StatusSnapshot {
        monitoring: accessible && nc.is_some(),
        accessibility: if accessible {
            "✓ Enabled"
        } else {
            "✗ Missing"
        },
        notification_center: if nc.is_some() {
            "✓ Connected"
        } else {
            "✗ Not found"
        },
        webhook: "Not tested (run --test-webhook)",
        webhook_url_redacted: sanitize_url_for_logging(&config.webhook_url),
        ..Default::default()
    };
    println!("{}", snapshot.render_text());
    if menu {
        println!("--- menu ---\n{}", snapshot.render_menu());
    }
    println!("Webhook URL: {}", snapshot.webhook_url_redacted);
    if let Some(pid) = nc {
        println!("Notification Center PID: {pid}");
    }
    Ok(())
}
