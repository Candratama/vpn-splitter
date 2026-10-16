# VPN Splitter desktop — draft architecture

Status: proposed for review, 2026-10-15. No desktop implementation yet.

## Goal and scope

A small macOS menu bar / Windows system tray app that connects existing OS VPN
profiles and edits the destinations that must use VPN. Public internet should
continue through the physical network. Start with the existing L2TP workflow.

Assumptions: one managed VPN connection at a time; macOS first, then Windows;
VPN profiles and credentials already exist in the OS. Creating profiles and
adding other VPN protocols are separate work. Profile names and connection
identifiers come from the current machine, never from repository defaults.

Initial validation platforms: macOS 14+ on Apple Silicon and Windows 11 x64.
Other architectures remain unverified until separately built and exercised.
Connection timeout is 30 seconds; one helper request has a 10-second timeout.
Refresh hostname routes every 5 minutes only while a session is active, with
immediate refresh on network change. Limit one plan to 4,096 expanded routes and
one IPC message to 1 MiB. Errors preserve the previous valid configuration.

## Technology decision

Selected: Rust + upstream GPUI for Settings and Diagnostics; native menus through
`gpui-tray`. No WebView, JavaScript runtime, frontend build, or local HTTP server.
Keep the GPUI application/event loop alive with zero visible windows. Create
Settings on demand and retain one window for hide/reveal on close. This lifecycle
amendment was approved on 2026-10-16 after destroy/recreate retained IOSurface
allocations. Verify stable window identity, native visibility, bounded memory and
suspended rendering while hidden. Release the window at process exit.

Use one pinned compatible GPUI source/revision across the dependency graph.
`gpui-tray` is a community dependency; its menu-state feature and upstream GPUI
revision must match. Commit Cargo.lock and validate a clean build before adding
UI components. Start with GPUI primitives; add a component library only if the
Settings editor demonstrably needs it.

A first release-mode prototype must prove: tray with no window, Settings open /
hide / reveal, enabled and disabled actions, callbacks on the GPUI foreground
executor, continued operation after the last window closes, explicit quit, and
macOS/Windows lifecycle compatibility. Perform blocking VPN/IPC/file work off the
UI executor and marshal state updates back to it.

Alternatives considered: Tauri for shared web-based Settings; separate AppKit/Win32
forms for native controls; Go + Wails for a Go core and WebView UI. GPUI is selected
for a single Rust stack. Its GPU rendering does not guarantee lower idle RAM,
CPU, startup time, or battery usage; measure those in release builds.

## Components and ownership

1. Desktop process: native tray/menu; on-demand GPUI Settings window; config
   validation and saving; connection progress; notifications. Runs as the user.
2. Rust control logic: one serialized connection lifecycle, destination planning,
   native VPN adapter, and OS event subscriptions. Shared by the tray and Settings.
3. Privileged helper: installs/removes approved destination routes, validates
   route plans independently, and owns durable route journals and crash cleanup.
   Runs only for installation/recovery or a managed routing session; no continuous
   daemon work when disconnected. Install/activate with the platform's admin UI.

These are two executables, not separate backend services. Keep shared code in a
small Rust workspace; use platform modules instead of a plugin architecture.

On macOS, use a launchd-managed helper with authenticated local IPC and native
VPN control through SystemConfiguration for supported saved profiles. On Windows,
use a service helper with an ACL-restricted named pipe, RAS for supported saved
VPN profiles, and IP Helper APIs for routing. Validate the calling identity and
session on every request. Production macOS helper installation includes code
signing and the appropriate service registration mechanism.

The helper accepts bounded operations such as apply a validated route plan,
clear a session, and read session status. It cannot execute arbitrary shell
commands, accept caller-selected journal paths, or load remote code. The UI calls typed Rust operations; no remote UI content or shell command
interface is exposed. IPC still requires independent helper validation.

## Routing and connection lifecycle

Use OS split tunneling: the saved VPN profile must not install a default route
through the tunnel. In v1, verify this prerequisite and guide the user to the OS
profile setting when necessary. Fail visibly if the profile's routing behavior
cannot be verified. Do not silently rewrite profile settings.

Add only explicit destination routes through the detected VPN interface. Never
add internet-wide /1 routes or replace the physical default route. This changes
the script's current routing strategy; the shell script is a reference, not the
desktop execution engine. It is not established that /1 routes directly caused
the previously observed socket error.

Connect sequence:

1. Validate config and split-mode prerequisite; snapshot the current physical
   route, resolver paths, and existing VPN state.
2. Start the selected saved VPN; await its OS connection event with a timeout.
   Identify the actual interface/session, never assume ppp0 or a stable index.
3. Recheck the physical default path, VPN endpoint reachability, DNS behavior,
   and profile split mode. Abort and disconnect an app-started connection on
   incompatible full-tunnel behavior.
4. Resolve configured hostnames using the appropriate OS resolver. Prepare
   exact routes, detect conflicts, and obtain helper validation before changes.
5. Persist each operation's intent before mutation, then its outcome. Preserve
   pre-existing routes. Read back applied routes before reporting active.
6. Confirm the public route remains physical. Optional bounded connectivity
   probes report degraded status, with separate public and internal results.

All operations are serialized. Cancel, timeout, partial apply, config replacement,
and OS disconnect use the same cleanup path. A config save while connected
validates and computes a replacement plan before mutating the current routes.
Restore the previous plan on a failed replacement when the session is still valid.

On Wi-Fi/Ethernet changes, wake, VPN loss, or app IPC closure, the helper removes
only routes still matching its recorded destination, gateway, and interface.
Changed or conflicting routes require reconciliation and are never blindly
deleted. On recovery, uncertain pending operations are reconciled before mutation.
Failed cleanup remains journaled, visible, and retryable. A helper restart checks
unfinished sessions before accepting another apply.

The desktop subscribes to OS VPN/network notifications; it does not poll every
second. It never reports routing active solely because an old state file exists.
One process instance and one managed session prevent competing route changes.

## DNS and address families

v1 supports IPv4 addresses, CIDRs, and explicit hostnames, including local import
of config.txt. Reject invalid addresses, /0 or internet-wide catch-all rules,
ambiguous conflicting routes, and URLs. Re-resolve hostnames on connect, network
change, and bounded periodic refresh while connected. Hostname routes can lag DNS
changes between refreshes; present this limitation in Settings.

Preserve OS resolver selection. Use the VPN's configured internal resolver when
available; a profile requiring unsupported DNS changes fails preflight with a
clear message. Do not copy the script's DNS route pinning without validating its
lifecycle. DNS preference must not be changed globally as a hidden side effect.

An arbitrary wildcard such as *.office.example cannot be expanded into a complete
hostname/IP inventory. v1 imports known hostnames and explicitly does not claim
complete wildcard coverage. If complete suffix coverage is required for the first
release, add a separately scoped design for domain-specific DNS forwarding plus
observed-answer routing, including TTLs, cached answers, DoH bypass, and shared-IP
limitations, before implementation. DBX inventory import does not require MCP at
runtime: import a local sanitized inventory file instead.

v1 is explicitly IPv4-only. AAAA-only destinations are unsupported; dual-stack
hostnames may bypass IPv4 routes. Do not label such destinations fully protected.
IPv6 internal coverage must be designed and verified before claiming all traffic
for a domain uses VPN; do not disable IPv6 globally as a workaround.

## User interface

Tray menu: current state, select saved profile, Connect/Disconnect, Refresh routes,
Settings, Diagnostics, and Quit. State uses icon shape plus text, not color alone.
Connecting/disconnecting disables conflicting actions. A routing failure is shown
separately from VPN connected, with Retry cleanup when needed.

Settings is a compact window: saved VPN profile picker; destination editor with
inline validation and import/export; launch-at-login option; save/apply status.
Diagnostics shows connection/interface, route verification, DNS result, and last
actionable error. Technical detail is available on demand.

Closing Settings hides the retained window and leaves the tray running. Quit clears managed routes and
disconnects a connection started by the app. A VPN that was already connected
before adoption remains connected; this behavior is stated in the Quit action.
If cleanup fails, retain the helper and journal and show the failure before exit.
On app crash, the helper clears owned routes. Disconnecting an app-started VPN
from the helper requires a verified user-session adapter; until that exists, keep
the OS VPN connected in split mode and report it on next launch. Do not promise
that route cleanup also disconnects the VPN.

## Storage and private data

Use versioned JSON with atomic replacement in the OS application-data directory;
no database. Store only profile references, destinations, and UI preferences.
Passwords and VPN secrets remain in OS credential storage; no credentials in
config or logs. Missing credentials open the OS configuration flow in v1.

Helper journals live in protected OS application state, not TMPDIR. Bound local
logs, redact destinations and profile names from exported diagnostics by default,
and require explicit inclusion for an export containing infrastructure details.
Repository examples use documentation addresses and example domains only.

## Performance budgets and validation

Initial targets, to be measured on named reference machines in release builds:

- Tray available within 1 second on warm launch, excluding admin/setup prompts.
- Desktop plus idle helper private memory below 50 MB with Settings hidden;
  total process memory below 150 MB while Settings is open.
- Average idle CPU below 0.5% of one core over five minutes; no recurring network
  probes or rendering loop while disconnected.
- Warm Settings opening below 500 ms; VPN handshake time measured separately.

Measure the full process tree, including helper processes, and report the OS
memory metric. If the tray-only GPUI build misses the idle budget, stop before building
Settings and report measurements plus a smaller native-tray alternative for
review. Do not silently switch frameworks.

Required validation covers partial route failure, missing admin permission,
disconnect during apply, UI/helper crash, helper restart, suspend/wake, network
switch, DNS failure, pre-existing/conflicting routes, malformed config, IPC access
control, and public/internal connectivity. macOS and Windows require independent
live verification; passing mocked OS tests does not establish platform support.

## Delivery order

1. GPUI tray/window lifecycle and resource proof; then macOS saved L2TP profiles,
   permission flow, recovery journal, and destination routing.
2. Settings editor, import, diagnostics, packaging, and signed macOS installer.
3. Windows adapters/helper, independent lifecycle checks, and installer.

No cloud sync, account system, embedded VPN protocol implementation, automatic
infrastructure discovery, or updater in the initial release. This revision selects GPUI at the user's request. The accompanying implementation
plan is for review before product code is written.

## References

- GPUI: https://gpui.rs/
- GPUI rendering/platforms: https://gpui.rs/examples/
- GPUI native tray integration and version compatibility: https://github.com/domenkozar/gpui-tray
- Apple VPN connection API: https://developer.apple.com/documentation/systemconfiguration/scnetworkconnectionstart(_:_:_:)
- Windows VPN/RAS: https://learn.microsoft.com/en-us/windows/win32/rras/virtual-private-network-connections
- Windows split-tunnel setting: https://learn.microsoft.com/en-us/powershell/module/vpnclient/set-vpnconnection
