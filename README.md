# macOS Notification Forwarder

<p align="center">
  <img src="assets/icon.png" width="128" height="128" alt="macOS Notification Forwarder Logo" />
</p>

<p align="center">
  <strong>Native Rust menu-bar application that intercepts macOS Notification Center banners and forwards them to a configurable HTTP webhook.</strong>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-macOS%2013%2B-black?style=flat-square&logo=apple" alt="macOS 13+" />
  <img src="https://img.shields.io/badge/language-Rust%202021-orange?style=flat-square&logo=rust" alt="Rust 2021" />
  <img src="https://img.shields.io/badge/ui-Native%20AppKit%20(objc2)-blue?style=flat-square" alt="Native AppKit" />
  <img src="https://img.shields.io/badge/license-GNU-green?style=flat-square" alt="License" />
</p>

---

## Overview

**macOS Notification Forwarder** captures notifications delivered to macOS Notification Center—including notifications forwarded via **iPhone Mirroring** (such as WhatsApp, Telegram, banking alerts, and SMS)—using the native macOS Accessibility API (`AXObserver`) and forwards them as structured JSON payloads to any HTTP webhook endpoint.

Built with **pure Rust** and native macOS AppKit via `objc2`:
- 🚫 **No Swift helper process**
- 🚫 **No Node.js / Electron**
- 🚫 **No Tauri / WebViews**
- 🚫 **No Hammerspoon**
- 🚫 **No CPU-draining polling loops**

```text
iPhone / macOS App ──► macOS Notification Center ──► AXObserver (Event Stream)
                                                            │
                                                     (250ms Debounce)
                                                            │
                                                            ▼
                                                  Targeted Banner Scanner
                                                            │
                                                            ▼
                                                 Banner Parser & Cleaner
                                                            │
                                                            ▼
                                                 App Filter (All / Selected)
                                                            │
                                                            ▼
                                                 SHA-256 Deduplication
                                                            │
                                                            ▼
                                                  Bounded Dispatch Queue
                                                            │
                                                            ▼
                                                 HTTP POST (+ Retry Backoff)
                                                            │
                                                            ▼
                                                   Your Webhook Endpoint
```

---

## Key Features

### 🎛️ Native AppKit Menu-Bar Agent & UI
- **Menu-Bar Tray Item**: Dynamic `bell.fill` status icon with tooltip, live status indicator (`● Monitoring`, `○ Paused`, `⚠ Permission required`), daily notification counters, and quick actions (Open, Pause, Test Webhook, Settings, Quit).
- **3-Tab Status Window**:
  - **Activity**: Real-time status hero card, pause/resume button, 4 live metric counters (**Forwarded**, **Total Today**, **Duplicates**, **Filtered**), and a scrollable recent notification feed.
  - **Webhook & Filters**: Configure webhook endpoint URL, test connections live, edit multi-line custom HTTP headers (e.g. `Authorization: Bearer <token>`), and toggle between *All Applications* or *Selected Apps* allowlist.
  - **Diagnostics**: One-click detection tests, live Accessibility hierarchy tree dump (`AXTree`), direct shortcuts to macOS System Settings, and a realtime auto-scrolling log console.
- **Pure Native Agent**: Configured with `LSUIElement = true`—runs unobtrusively in the menu bar without cluttering the Dock or window switcher.

### 📱 Full iPhone Mirroring & Mobile Alert Support
- Automatically detects notifications mirrored from your iPhone (e.g. WhatsApp, Messages, banking applications like Booking, Telegram, Slack).
- **Sanitization & Extraction**:
  - Strips invisible Unicode bidirectional formatting marks (`\u{200e}`, `\u{200f}`, etc.).
  - Decodes spoken VoiceOver/AX summaries (e.g. `\u{200e}WhatsApp, Niles 💟, Message`).
  - Ignores generic container labels (`Notification Center`, `Control Center`) so the genuine originating app name is always identified.

### ⚡ Zero-Polling Event Engine
- Uses macOS `AXObserver` notifications (`AXCreated`, `AXUIElementDestroyed`, `AXValueChanged`, `AXTitleChanged`, `AXWindowCreated`, `AXLayoutChanged`).
- Sleeps while idle; event bursts are coalesced with a configurable debounce timer (default `250ms`) before scanning visible banners.
- Auto-reconnects automatically if Notification Center restarts or crashes.

### 🔒 Deduplication & Security
- **Content-Based Fingerprinting**: macOS Notification Center frequently reuses existing accessibility UI elements. Identity is computed via `SHA256(app_name + "\0" + title + "\0" + message)`.
- **Bounded TTL Cache**: Retains fingerprints up to a configurable duration (default 15 minutes) within a bounded cache to prevent duplicate webhook dispatches while allowing repeated messages over time.
- **Privacy & Secret Protection**:
  - Configuration saved with `0o600` permissions.
  - Query parameters like `token`, `secret`, `key`, `auth`, `password`, and `signature` are automatically redacted in logs.
  - Custom HTTP header values are never exposed in log outputs.

### 🚀 Headless & Automation Ready
- Supports `--no-gui` headless mode for SSH sessions, background daemons, or `launchd` services.
- Rich CLI diagnostic flags for testing detection, inspecting AX elements, and validating webhooks.

---

## Workspace Architecture

The project is structured as a modular Rust Cargo workspace:

```text
macos-notification-forwarder/
├── assets/                  # App icons (PNG, 1024x1024 master, multi-res .icns)
├── crates/
│   ├── accessibility/       # AXObserver stream, NC PID discovery, targeted banner scanner
│   ├── app/                 # Main entrypoint, native AppKit GUI (objc2), CLI, worker thread
│   ├── config/              # JSON config (0o600 perms), path resolution, secret redaction
│   ├── notification/        # Pure engine: parser, bidi cleaner, dedup (SHA-256), app filter
│   └── webhook/             # Async HTTP client, retry policy, bounded dispatch queue
├── packaging/
│   ├── build-app.sh         # App bundle builder + ad-hoc codesigning script
│   ├── Info.plist           # Bundle manifest (LSUIElement=true, permissions description)
│   └── com.qya.notification-forwarder.plist  # LaunchAgent plist for Launch at Login
├── Cargo.toml               # Workspace root configuration
└── README.md
```

---

## Prerequisites

- **macOS 13.0+** (Ventura, Sonoma, Sequoia, or later).
- **Rust stable** (install via [rustup](https://rustup.rs/)).
- **Xcode Command Line Tools** (`xcode-select --install`).  
  *(Full Xcode is **not** required for the default native AppKit GUI!)*
- **Accessibility Permission**:
  - For the `.app`: `System Settings → Privacy & Security → Accessibility → Notification Forwarder`.
  - For terminal execution: Grant permission to your terminal emulator (e.g. Terminal, iTerm2, Alacritty, Ghostty).

---

## Installation & Packaging

### 1. Build the macOS `.app` Bundle

A turnkey build script is provided to assemble and ad-hoc sign `NotificationForwarder.app`:

```sh
# Release build (default)
./packaging/build-app.sh

# Or debug build (faster compilation)
./packaging/build-app.sh --debug
```

The output bundle will be generated at `dist/NotificationForwarder.app`.

### 2. Launching and Installing

You can run the app directly from `dist/`:

```sh
open dist/NotificationForwarder.app
```

Or install it to `/Applications`:

```sh
cp -R dist/NotificationForwarder.app /Applications/
open /Applications/NotificationForwarder.app
```

On first launch, if Accessibility permission has not yet been granted, the app will prompt you with an alert and provide a direct shortcut to open macOS System Settings.

### 3. Launch at Login (Optional)

To have Notification Forwarder start automatically upon login using `launchd`:

```sh
cp packaging/com.qya.notification-forwarder.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.qya.notification-forwarder.plist
```

To unload/disable:
```sh
launchctl unload ~/Library/LaunchAgents/com.qya.notification-forwarder.plist
```

---

## Command Line Usage

You can run the application directly from source via `cargo`:

```sh
# Default: Launch native AppKit menu-bar agent + status window
cargo run

# Headless mode: run monitor loop in terminal (logs to stderr, no GUI window)
cargo run -- --no-gui

# Scan visible banners once, forward matching notifications, and exit
cargo run -- --once

# One-shot detection test: scan and print parsed notifications without sending
cargo run -- --test-detection

# AX tree dump: print the Notification Center Accessibility tree hierarchy
cargo run -- --dump-tree

# Test webhook: send a test payload to verify endpoint connectivity
cargo run -- --test-webhook

# Diagnostics snapshot: check Accessibility status, NC connection, and webhook config
cargo run -- --status
cargo run -- --status --menu   # also print menu-bar snapshot
```

### Command Line Options

| Flag | Description |
|---|---|
| `--no-gui` | Runs the headless monitor loop (no AppKit window or menu bar item). |
| `--config <PATH>` | Override configuration file path (or use `$NOTIFORWARDER_CONFIG`). |
| `--webhook-url <URL>` | Override destination webhook URL for this run. |
| `--test-detection` | Perform a single scan and print what would be forwarded. |
| `--test-webhook` | Dispatch a test payload to the configured webhook and exit. |
| `--dump-tree` | Dump the Notification Center accessibility element tree to stdout. |
| `--once` | Scan once, forward any pending banners, and exit immediately. |
| `--status` | Display diagnostic status (TCC, Notification Center PID, Webhook status). |
| `--menu` | Include the menu-bar tray status text in `--status` output. |
| `-v`, `--verbose` | Enable debug logging (or set `RUST_LOG=debug`). |
| `-h`, `--help` | Display usage instructions and options. |

---

## Configuration

The configuration file is stored locally with secure `0o600` permissions (readable/writable only by your user):

- **Primary path**: `~/Library/Application Support/NotificationForwarder/config.json`
- **Fallback path**: `~/.config/notification-forwarder/config.json`
- **Override**: Pass `--config /path/to/config.json` or set `NOTIFORWARDER_CONFIG=/path/to/config.json`.

### Example `config.json`

```json
{
  "webhook_url": "https://api.yourdomain.com/notifications/webhook",
  "webhook_headers": {
    "Authorization": "Bearer secret_api_token_here",
    "X-Source": "macos-notification-forwarder"
  },
  "forwarding_enabled": true,
  "diagnostic_logging": false,
  "start_at_login": false,
  "filter_mode": "selected",
  "allowed_apps": [
    "WhatsApp",
    "Messages",
    "Slack",
    "Booking"
  ],
  "dedup_ttl_secs": 900,
  "dedup_capacity": 2000,
  "debounce_ms": 250,
  "max_attempts": 3
}
```

### Configuration Options

| Key | Type | Default | Description |
|---|---|---|---|
| `webhook_url` | String | `""` | Destination HTTP POST endpoint. |
| `webhook_headers` | Object | `{}` | Key-value pairs sent as HTTP headers (e.g. auth tokens). |
| `forwarding_enabled`| Bool | `true` | Master switch to enable or pause forwarding. |
| `diagnostic_logging`| Bool | `false` | Enables verbose logging of banner contents. |
| `filter_mode` | String | `"all"` | `"all"` to forward all apps, or `"selected"` to use `allowed_apps`. |
| `allowed_apps` | Array | `[]` | List of application names allowed when `filter_mode` is `"selected"`. |
| `dedup_ttl_secs` | Number | `900` | Deduplication window in seconds (15 minutes). |
| `dedup_capacity` | Number | `2000` | Maximum number of notification fingerprints cached in memory. |
| `debounce_ms` | Number | `250` | Debounce delay between AX event and targeted scan in milliseconds. |
| `max_attempts` | Number | `3` | Maximum attempts for webhook delivery before giving up. |

---

## Webhook Payload Specification

Notifications are dispatched as HTTP `POST` requests with `Content-Type: application/json`:

```http
POST /notifications/webhook HTTP/1.1
Host: api.yourdomain.com
Content-Type: application/json
Authorization: Bearer secret_api_token_here
X-Source: macos-notification-forwarder

{
  "app_name": "WhatsApp",
  "title": "Niles 💟",
  "message": "Hey! Are you available for the call?",
  "notification_id": "79BAE16D-4521-4E10-8B55-0A74B8D56A12",
  "timestamp": "2026-09-10T17:30:00+07:00"
}
```

### Payload Fields

| Field | Type | Description |
|---|---|---|
| `app_name` | String | Originating application name (e.g. `WhatsApp`, `Messages`, `Slack`). |
| `title` | String | Notification title or sender name. |
| `message` | String | Notification body text (may be empty for title-only banners). |
| `notification_id` | String \| null | Accessibility element identifier if provided by macOS; `null` otherwise. |
| `timestamp` | String | RFC 3339 timestamp with local timezone offset. |

---

## Development & Testing

### Running Tests

The test suite covers parsing heuristics, deduplication caches, retry policies, header validation, and security sanitization:

```sh
# Run all workspace unit tests
cargo test --workspace
```

### Testing Detection Locally

1. Open your terminal and run:
   ```sh
   cargo run -- --test-detection
   ```
2. Trigger a notification on your Mac (or via iPhone Mirroring).
3. The command will output the parsed application name, title, message, and whether your current filter would forward or drop it.

### Inspecting Notification Center Accessibility Tree

To inspect the live AX hierarchy structure exposed by macOS Notification Center:

```sh
cargo run -- --dump-tree
```

---

## Optional: GPUI Layer

An optional presentation layer using [GPUI](https://github.com/zed-industries/gpui) is maintained behind the `gpui-ui` feature flag:

```sh
cargo run --features gpui-ui
```

> **Note**: Building with `--features gpui-ui` requires a full Xcode installation (`xcrun metal`) because GPUI compiles Metal shaders during build time. The default AppKit GUI requires only the lightweight Command Line Tools.

---

## Acceptance Criteria Mapping

| ID | Criterion | Verification & Implementation |
|---|---|---|
| **AT-01** | Accessibility permission granted transitions to active monitoring | `crates/app/src/main.rs` (`wait_until_trusted`), `nf_accessibility::accessibility_trusted` |
| **AT-02** | Structured banner parsing (e.g. WhatsApp `Niles 💟` / `AAA`) | `crates/notification/src/parser.rs` (`parser::tests::whatsapp_example`) |
| **AT-03** | Repeated banners within TTL window yield only one webhook dispatch | `crates/notification/src/dedup.rs` (`dedup::tests::duplicate_sequence`) |
| **AT-04** | Updated banner content (`AAA → BBB`) re-fires webhook | `crates/notification/src/dedup.rs` (content SHA-256 fingerprinting) |
| **AT-05** | Structured JSON payload delivered via HTTP POST | `crates/webhook/src/client.rs` (`WebhookPayload`, `WebhookClient::send`) |
| **AT-06** | Notification Center crash/restart automatically rediscovers PID & reconnects | `crates/accessibility/src/observer.rs` (`ObserverEngine::run` recovery loop) |
| **AT-07** | Permission revocation pauses monitoring and displays permission screen | `crates/app/src/main.rs`, `crates/app/src/gui.rs` (`apply_permission_state`) |
| **AT-08** | Application allowlist filtering drops unlisted apps | `crates/notification/src/filter.rs` (`filter::tests::selected_mode_filters`) |

---

## License
licensed under the [GNU General Public License v3.0 only](LICENSE).