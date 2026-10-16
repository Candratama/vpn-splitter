# Rust GPUI VPN Splitter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a small tray app that edits destination rules and manages saved OS VPN connections while preserving public internet access.

**Architecture:** GPUI owns on-demand Settings/Diagnostics and native tray actions. A Rust core serializes connection changes; an independently validating privileged helper owns route mutations and recovery. Start with a measured tray prototype, ship macOS first, then validate Windows independently.

**Tech Stack:** Rust, upstream GPUI, compatible gpui-tray, serde/serde_json, platform bindings for SystemConfiguration/AppKit/ServiceManagement and Windows RAS/IP Helper/service APIs. No WebView, Node, local HTTP server, database, or shell-script execution engine.

**Spec:** `docs/desktop-architecture.md` (read before this plan).

## Global Constraints

- Initial validation platforms: macOS 14+ on Apple Silicon and Windows 11 x64.
- One managed VPN connection at a time; use existing saved L2TP profiles and OS credentials.
- Connection timeout is 30 seconds; one helper request has a 10-second timeout.
- Refresh hostname routes every 5 minutes only while a session is active, with immediate refresh on network change.
- Limit one plan to 4,096 expanded routes and one IPC message to 1 MiB.
- IPv4 only; explicitly disclose wildcard and dual-stack limitations.
- Never add internet-wide /1 routes or replace the physical default route.
- Preserve pre-existing routes, OS DNS settings, and connections not started by this app.
- Treat an incomplete cleanup as an error; retain its protected recovery journal.
- No real infrastructure destinations, profile names, credentials, logs, or inventories in tracked files.
- Keep the current CLI intact. Use conventional commits with explicit file staging.
- Product implementation starts after this plan is reviewed; no automatic push or release.

## Review Focus

- A callback from a vanished session must not change the current session: Task 3 tests session identity and cancellation.
- A stale journal or route with the same destination but changed owner must not delete someone else's route: Task 4 tests reconciliation.
- Closing/reopening Settings and launching a second app instance must retain one tray/controller: Tasks 1 and 7 verify this live.
- An OS profile renamed/deleted or credentials requiring interaction must produce actionable errors without reconnect loops: Tasks 2 and 6 exercise this.
- A helper caller with the wrong identity or an oversized/truncated message must not mutate routes: Tasks 5 and 9 exercise this.

## File map and shared contracts

The final root Cargo workspace has three members. Add each member when its task starts:

- `crates/core/src/{lib,config,session,routing,protocol}.rs`: shared typed config, lifecycle, route policy, IPC data. No GUI or privileged OS calls.
- `crates/desktop/src/{main,tray,settings,diagnostics}.rs`: GPUI app, native menus, UI state, explicit exports.
- `crates/desktop/src/platform/{mod,macos,windows}.rs`: saved-profile operations, OS event subscriptions, single-instance and login integration.
- `crates/helper/src/{main,journal,service}.rs`: request validation, serialized transactions, durable recovery and IPC/session lifecycle.
- `crates/helper/src/platform/{mod,macos,windows}.rs`: authenticated OS transport, protected state, native route API, helper registration.
- `packaging/macos/` and `packaging/windows/`: app/service metadata and installation assets created only when packaging is implemented.
- `docs/desktop-validation.md`: sanitized live validation evidence, reference machines and measured resource metrics.

Use inline Rust unit tests for pure policy. Add `crates/helper/tests/recovery.rs` for crash-point recovery and platform integration checks only when their adapters exist. Do not create empty modules for later tasks.

Shared types are introduced by the owning task below. Public operations return `Result<T, Error>`; `Error` is a typed core enum with stable codes for invalid input, permission, timeout, conflict, unsupported profile, DNS, transport and cleanup failure. Preserve private OS detail locally; show a safe actionable message in the UI.

## Milestone A — measured tray and safe control model

### Task 1: Prove GPUI tray/window lifecycle before committing to the UI

**Files:** Create a desktop-only root Cargo workspace, desktop `Cargo.toml`, `Cargo.lock`, `crates/desktop/src/{main,tray}.rs`; modify `.gitignore`; create `docs/desktop-validation.md`.

**Interfaces:** Produce `tray::install(cx: &mut gpui::App) -> Result<Tray, Box<dyn std::error::Error>>`; desktop retains the tray handle for process lifetime. Prototype actions are Open Settings and Quit; unavailable VPN actions stay disabled.

- [x] Resolve and pin one GPUI revision supported by gpui-tray, including checked/disabled menu state. Verify one GPUI source with `cargo tree -d`; record revision/toolchain and license requirements. Do not add a component library.
- [x] Build a zero-window app and a minimal Settings window reused through native hide/reveal. Intercept close without destroying it; verify one stable GPUI handle and correct native visibility across 20 cycles. Keep the GPUI application alive after its last window closes. Implement single-instance locking with an OS-held lock, not a stale PID file. Handle menu callbacks on the foreground executor; run blocking work off it.
- [x] Run `cargo build --release -p vpn-splitter-desktop`. Live-check tray-only launch, open/close/reopen 20 times, checked/disabled actions, second launch, and Quit. At this stage a second instance exits safely; focus forwarding can be added with Settings.
- [x] Measure warm startup, Settings opening, idle CPU for five minutes and complete process-tree memory before first Settings creation and after repeated hide/reveal. Targets: tray <1 s, Settings <500 ms, idle private memory <50 MB, open Settings total memory <150 MB, idle CPU <0.5% of one core. Record hardware, OS, metric and result; do not claim GPU resources are released without evidence. A missed budget or unsupported tray lifecycle stops expansion for design review.
- [x] Commit only workspace/prototype/docs changes: `feat: add GPUI tray lifecycle prototype`.

**2026-10-16 checkpoint:** destroy/recreate failed the idle-memory gate. User approved reuse of one hidden Settings window; Task 1 passed the measured macOS prototype gate with one retained hidden window. Windows runtime checks remain in Tasks 9–10. See `docs/desktop-validation.md`. Task 2 is next.

### Task 2: Define validated local configuration

**Files:** Add the core workspace member (`Cargo.toml`, `src/lib.rs`, `src/config.rs`); define the shared `Error` enum here and add core dependencies.

**Interfaces:** Produce `Config { version: u32, profile_id: Option<String>, destinations: Vec<Destination>, launch_at_login: bool }`, `Destination::{Ipv4, Cidr, Hostname}`, `Config::parse(&str) -> Result<Config, Error>`, `import_rules(&str) -> Result<Vec<Destination>, Error>`, `load_config(&Path) -> Result<Config, Error>` and `save_config(&Path, &Config) -> Result<(), Error>`.

- [ ] Write failing inline tests: invalid IPv4 octets/prefixes, URLs, wildcard, IPv6, unknown schema version, duplicate/case-insensitive hostnames, forbidden /0 and catch-all /1, 4,097 rules, comments/blank lines from config.txt. Assert bad input returns an error and never overwrites the previous file.
- [ ] Run `cargo test -p vpn-splitter-core config` and confirm failures come from missing policy. Implement version 1 JSON, normalized/deduplicated rules, private OS app-data location selected by the desktop, and atomic replacement in the same directory with platform-correct durability. A missing config gives empty defaults; corrupt config is visible and preserved.
- [ ] Run `cargo test -p vpn-splitter-core config`; simulate interrupted saves and assert the old or new complete JSON remains. Keep example rules in reserved documentation ranges/example domains. No secrets or machine-specific defaults.
- [ ] Commit: `feat: add validated local VPN configuration`.

### Task 3: Define the serialized session and route plan

**Files:** Create `crates/core/src/{session,routing,protocol}.rs`; extend core exports.

**Interfaces:** Produce `ProfileId`, unique `SessionId`, `SessionState::{Disconnected, Connecting, Applying, Active, Disconnecting, CleanupRequired}`, `Session::handle(Event) -> Vec<Effect>`. Events/effects carry session identity; effects request OS/helper work and return completion events. Define `Route { destination, prefix, interface_id, gateway }`, `NetworkSnapshot`, `RoutePlan`, and `build_plan(&Config, &NetworkSnapshot, &[ResolvedHost]) -> Result<RoutePlan, Error>`. Protocol v1 provides `Request::{Begin, Apply, Clear, Status}` and corresponding typed replies; Begin binds peer, session and verified VPN interface.

- [ ] Add failing tests for duplicate Connect, Disconnect during Apply, late Connected/Applied callbacks, 30 s connection timeout, 10 s helper timeout, DNS failure, route-limit expansion, full-tunnel profile, missing profile and conflicting routes. Assert defaults, /1 internet routes and DNS mutations never appear in effects/plans.
- [ ] Run `cargo test -p vpn-splitter-core`; implement one deterministic state machine driven by events, with an injected monotonic clock for tests. Begin/Apply require a matching live session, interface verification and successful split-mode checks. An old file alone never implies Active.
- [ ] Add replacement-plan tests: validate/resolve before mutation, rollback to the prior plan after failure if the session is valid, otherwise clear and show CleanupRequired. Preserve whether the VPN was app-started or already connected; only app-started connections receive normal Quit/Disconnect effects.
- [ ] Run `cargo test -p vpn-splitter-core`; commit: `feat: add serialized VPN session and route planning`.

## Milestone B — macOS helper and working VPN

### Task 4: Implement recoverable route transactions

**Files:** Add the helper workspace member (`Cargo.toml`, `src/main.rs`, `src/lib.rs`, `src/journal.rs`) and `crates/helper/tests/recovery.rs`.

**Interfaces:** Consume `SessionId`, `RoutePlan`, `Route`. Produce `Journal::open(&Path) -> Result<Journal, Error>`, `apply(&mut Journal, &RoutePlan, &mut impl RouteBackend) -> Result<(), Error>`, `clear(&mut Journal, &mut impl RouteBackend) -> Result<CleanupReport, Error>` and `reconcile(&mut Journal, &mut impl RouteBackend) -> Result<CleanupReport, Error>`. `RouteBackend` is the one OS boundary with read/add/delete methods; production and in-memory fault injection are its implementations. `CleanupReport` distinguishes removed, missing, externally changed and failed routes.

- [ ] Add failing recovery tests for failure after each journal write or OS mutation, pre-existing identical route, conflicting route, externally changed gateway/interface, already missing route, corrupted journal and delete denied. Assert no unowned route is deleted, pending work is reconciled, and failed cleanup remains durable.
- [ ] Run `cargo test -p vpn-splitter-helper --test recovery`. Implement intent-before-mutation, durable completion records, exact current-route matching, serialized apply/clear and idempotent reconciliation. Journals live in helper-owned protected storage; reject unsafe paths/symlinks and do not trust caller paths.
- [ ] Repeat the recovery tests with journal reload between fault and recovery; require either the known previous plan or an explicit unresolved cleanup record. A corrupted journal blocks new mutations and exposes recovery diagnostics instead of guessing ownership.
- [ ] Commit: `feat: add recoverable owned-route transactions`.

### Task 5: Add macOS helper permissions, IPC and native route operations

**Files:** Create `crates/helper/src/{service,platform/mod,platform/macos}.rs`, `packaging/macos/` helper registration metadata; extend helper dependencies.

**Interfaces:** Consume protocol v1 and Task 4 transactions. Produce `serve() -> Result<(), Error>`, `authorize_peer(...) -> Result<PeerIdentity, Error>`, `MacRouteBackend`, and desktop-facing `HelperClient::request(Request) -> Result<Reply, Error>`. Pin the actual native binding crates in Cargo.lock after checking their APIs. macOS helper registration uses ServiceManagement/launchd and authenticated XPC; a narrow native bridge is permitted only where Rust bindings do not cover XPC lifecycle safely.

- [ ] Write boundary checks for unauthorized UID/code identity, replay/wrong session, wrong VPN interface, oversized (>1 MiB), malformed/truncated messages, invalid routes and concurrent clients. Assert rejection precedes any mutation; do not accept arbitrary executable names or shell strings.
- [ ] Implement admin registration/activation, protected helper state and native route operations. Validate profile/interface and route bounds independently in the helper. XPC invalidation or VPN/network loss initiates owned-route cleanup; monitor the relevant OS events while a session exists. Recover unfinished journals before accepting Begin. Stop session monitoring and idle work when disconnected.
- [ ] On a controlled test machine, verify permission denial makes no changes; authorized app can apply/remove documentation test routes; wrong identity cannot; killing desktop triggers cleanup; restarting helper reconciles partial work. Run `cargo test -p vpn-splitter-helper`. Avoid changing live user routes for tests without an explicit test session.
- [ ] Commit: `feat: add macOS privileged routing helper`.

### Task 6: Integrate saved macOS VPN profiles and network events

**Files:** Create `crates/desktop/src/platform/{mod,macos}.rs`; extend desktop controller wiring in `main.rs`/`tray.rs`.

**Interfaces:** Produce `list_profiles() -> Result<Vec<Profile>, Error>`, `connect(&ProfileId) -> Result<(), Error>`, `disconnect(&ProfileId) -> Result<(), Error>`, `snapshot(&ProfileId) -> Result<NetworkSnapshot, Error>`, `subscribe_events(Sender<Event>) -> Result<Subscription, Error>`, and `resolve_hosts(&[Destination]) -> Result<Vec<ResolvedHost>, Error>`. `Profile` includes stable OS ID, display name and supported/split-mode status; `Subscription` unregisters on drop. Adapt OS callbacks into Task 3 events.

- [ ] Add adapter/controller checks for deleted/renamed profiles, unsupported type, missing saved credentials, external VPN connection, failed DNS, interface replacement, network switch and sleep/wake. Assert user-managed VPN is not disconnected and no busy reconnect loop starts.
- [ ] Implement SystemConfiguration saved-profile control, dynamic-store/network event subscriptions, native resolver behavior and actual interface discovery. Reject unverifiable/full-tunnel profiles and direct users to OS configuration. OS UI handles missing credentials. Schedule five-minute hostname refresh only while active; debounce repeated network events and cancel obsolete resolution results by session ID.
- [ ] Wire the complete Connect → verify → resolve → helper Apply → readback sequence. Tray state is updated only from verified results. Normal Disconnect/Quit clears owned routes first and disconnects only an app-started VPN. Cleanup failure leaves a usable tray and retry action; app crash route cleanup does not claim guaranteed VPN disconnect.
- [ ] Live-check public HTTPS and an explicitly supplied internal destination before/during/after a saved L2TP session, full-tunnel refusal, cable/Wi-Fi change, cancel and failed cleanup. Record sanitized results; never add real config or inventory to Git.
- [ ] Run `cargo test --workspace` and commit: `feat: manage macOS VPN sessions from the tray`.

## Milestone C — usable macOS app

### Task 7: Add GPUI Settings, diagnostics and user actions

**Files:** Create `crates/desktop/src/{settings,diagnostics}.rs`; extend `tray.rs`, `main.rs` and macOS platform integration.

**Interfaces:** Consume `Config`, `SessionState`, `Profile` and controller commands. Produce `open_settings(cx: &mut gpui::App)`, `open_diagnostics(cx: &mut gpui::App)` and `export_diagnostics(include_infrastructure: bool) -> Result<String, Error>`. One live window per role; focus an existing window rather than duplicate it. The controller owns live state independently of either window.

- [ ] Test redacted export excludes names, destinations and raw OS errors; config validation errors preserve existing saved config; dual-stack/AAAA-only and wildcard input show limitations. Verify a second instance can focus Settings via authenticated user-level IPC without acquiring control ownership.
- [ ] Build compact Settings with profile picker, multiline destinations, import/export, launch-at-login and Save/Apply. Add tray state, profile selection, Connect/Disconnect, Refresh routes, Diagnostics, Retry cleanup and ownership-aware Quit label. Show VPN connected and routing active separately.
- [ ] Implement login registration through OS facilities, bounded local diagnostics and explicit infrastructure inclusion for export. Keep blocking file, DNS, VPN and helper operations off the GPUI foreground executor. Diagnostics renders the current controller snapshot and last actionable error, never reads journals directly.
- [ ] Live-check keyboard navigation, focus, readable labels, screen-reader behavior, light/dark contrast, import/save, error recovery and 20 Settings reopen cycles. If GPUI cannot meet required accessibility on a target, record a release blocker. Run `cargo test --workspace` and repeat resource measurements after real UI integration.
- [ ] Commit: `feat: add GPUI configuration and diagnostics`.

### Task 8: Package and qualify the macOS build

**Files:** Add `packaging/macos/` app/installer assets and `scripts/package-macos.sh`; update `README.md`, `.gitignore` and `docs/desktop-validation.md`.

**Interfaces:** Produce an app bundle and helper install/uninstall flow for macOS 14+ Apple Silicon. Read signing identities from the user's build environment; never embed or commit them. Missing signing credentials produce an explicitly unsigned local artifact, not a claimed distributable release.

- [ ] Implement release packaging, icons, bundle IDs, helper registration/signature relationship and explicit uninstall cleanup. Uninstall must retain/report unresolved route cleanup instead of blindly deleting its only journal. Keep generated bundles and build evidence out of tracked files.
- [ ] Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo build --workspace --release` and `bash test_vpn_splitter.sh`. Existing CLI regressions and new Rust failures block completion.
- [ ] Exercise install/admin denial/retry, app update with unfinished state, reboot recovery, crash, network switch, Quit and uninstall on a controlled Mac. Record private memory metric, RSS separately, GPU impact if measurable, startup and CPU budgets; mark unmeasured values unverified.
- [ ] Inspect the exact staged diff for real IPs, infrastructure domains, names and secret material. Commit: `build: package and qualify macOS desktop app`. No push/notarization upload/publication without the corresponding release instruction.

## Milestone D — Windows parity

### Task 9: Implement Windows saved VPN and helper adapters

**Files:** Create `crates/desktop/src/platform/windows.rs`, `crates/helper/src/platform/windows.rs`, `packaging/windows/` service metadata; extend target-specific Cargo dependencies.

**Interfaces:** Implement Task 6 platform functions with RAS and OS network notifications; implement Task 5 `HelperClient`/`serve` using ACL-restricted named pipes and a Windows service, and Task 4 `RouteBackend` using IP Helper APIs. Keep RAS authentication/control in the interactive user's session; do not assume the service has the user's phonebook or credentials.

- [ ] Port boundary/recovery tests to Windows, including wrong SID/session, pipe client impersonation/identity verification, malformed frames, service interruption, renamed adapter and per-user versus all-user profile lookup. Verify 1 MiB limits and concurrent client rejection.
- [ ] Implement supported saved L2TP profile enumeration, split-mode verification, asynchronous RAS operations, actual adapter discovery, notification subscriptions, protected ProgramData journals, atomic JSON replacement and service permission/install flow. Preserve a single compatible GPUI revision across macOS and Windows builds.
- [ ] Run `cargo test --workspace` and release build on Windows 11 x64. Live-check zero-window tray, Settings close/reopen, single instance, admin denial, app/service crash, reboot recovery, VPN loss and public/internal routes with a controlled saved profile. Cross-compilation alone is insufficient.
- [ ] Commit: `feat: add Windows VPN and routing adapters`.

### Task 10: Package and qualify Windows parity

**Files:** Complete `packaging/windows/`, add `scripts/package-windows.ps1`; update `README.md` and `docs/desktop-validation.md`.

**Interfaces:** Produce the Windows app + service installer and uninstall path, with optional launch-at-login. No embedded WebView runtime. Document exact executable/installer output paths after a real build.

- [ ] Build installer/service registration, login startup, icons and cleanup-aware uninstall. Preserve unresolved journals during failed removal. Support an unsigned local artifact if no signing identity is available; state its signing status precisely.
- [ ] Run format, clippy, workspace tests and release build on Windows; exercise installed behavior on a clean Windows 11 x64 machine. Repeat the complete resource/lifecycle matrix and store sanitized evidence next to macOS results. Installer generation is not proof of VPN support.
- [ ] Review all staged files for private data; commit: `build: package and qualify Windows desktop app`.

## Completion and handoff

The prototype, macOS release and Windows release are separate delivery checkpoints.
Do not claim Windows parity before Tasks 9–10 run on Windows. Missing test devices,
saved VPN credentials, signing identities or admin interaction are recorded as
specific unverified criteria, not replaced by mocks or successful compilation.

After plan approval, recommended execution is Native: one implementer follows the
dependent tasks in order; a fresh reviewer checks each release checkpoint before
delivery. Routing ownership, recovery and IPC need review before live integration.
Use an isolated worktree at execution time and preserve ignored local config in
the original checkout. No implementation was performed while writing this plan.
