# GPUI desktop prototype validation — 2026-10-16

Status: the initial destruction experiment failed; the approved reuse lifecycle
now passes the measured macOS resource gate. Core config validation/storage is
implemented and tested; VPN/session/helper integration remains pending. No VPN control, destination editor, privileged helper, installer, or
Windows support is claimed by this prototype.

## Environment and dependencies

- Mac14,2, Apple Silicon, 16 GiB RAM; macOS 27.0.1 build 26A434.
- rustc/cargo 1.97.1; other toolchain versions and macOS 14 runtime unverified.
- GPUI 0.2.2 and gpui_platform from Zed revision
  `8166e3d7b8b42d8aaf4d4dee7fcd25ab4ec65105`; gpui-tray 0.1.4.
- `cargo tree -d` inspected and `cargo tree -i gpui` confirms a single GPUI source.
- Direct GPUI/platform/tray packages declare Apache-2.0. Preserve applicable
  licenses/notices and inspect transitive license requirements before distribution.
- Xcode's optional Metal Toolchain was absent and installed using the official
  `xcodebuild -downloadComponent MetalToolchain` command (27A266a).
- Upstream `block` 0.1.6 emits a future Rust compatibility warning. Current
  compilation succeeds; a future toolchain upgrade requires review.

## Functional checks

- Default launch installs the tray without a Settings window or Dock icon.
- Native Settings rendering inspected. One window is reused while open; after
  closing it a newly created window can reopen.
- Twenty close/reopen cycles exercised through native UI automation. The tool's
  app activation triggers the OS reopen event, which dispatches the same Settings
  action. This is not described as twenty tray clicks.
- The user separately confirmed the native tray menu, Settings, and Quit work.
  The unavailable VPN menu item is disabled. Checked Settings state is supplied
  through gpui-tray's native menu-state integration; dedicated native checked-state
  readback remains unverified.
- Five external warm launches measured with a parent process stopwatch from
  process creation to the tray-installed log: 127.266, 118.581, 94.137, 87.211,
  83.907 ms. This includes process startup but not independent compositor timing.
- Five duplicate launches exit without taking control. The OS lock is released
  after each owner exits. The unit test was observed failing before OS locking
  was implemented, then passing.
- First Settings frame callback: 275.013 ms; subsequent measured opens approximately
  24–44 ms. This measures GPUI's frame callback, not compositor presentation.
- Initial Settings RSS: 92.8 MiB; charged physical footprint: 31.3 MiB.
- Release executable: 8,807,264 bytes (8.4 MiB), excluding OS libraries/tooling.

## Memory and CPU evidence

Read-only `scripts/measure-macos.py` samples macOS `proc_pid_rusage` v0 for the
root and descendants every five seconds. `vmmap -summary` cross-checks memory.
Physical footprint is charged process memory, including relevant GPU/compressed
allocations; it is not equated to anonymous private RAM. RSS is reported separately.
The intended idle budget is evaluated conservatively using charged footprint,
rather than overlooking retained GPU allocations when RSS alone looks smaller.

Before any Settings window, over 300.037 seconds:

- CPU average: 0.0004% of one core.
- Peak process-tree physical footprint: 12,862,328 bytes (12.3 MiB).
- Peak process-tree RSS: 53,739,520 bytes (51.3 MiB).
- No child process or helper was present.

After twenty native close/reopen cycles and final close, over 10.044 seconds:

- CPU average: 0.0001% of one core.
- Physical footprint: 166,200,376 bytes (158.5 MiB).
- RSS: 109,789,184 bytes (104.7 MiB).
- vmmap IOSurface resident allocation grew from about 3.9 MiB with the first
  Settings window to 85.9 MiB after repeated creation.

To rule out accessibility clients retaining the old windows, an independent
release-mode probe used the production Settings action and GPUI's window removal
API, with no accessibility client attached. It asserted exactly one GPUI window
while open and zero after every close, repeated twenty times:

```sh
cargo run --release --locked -p vpn-splitter-desktop --example window_lifecycle_probe
```

The probe leaves sixty seconds to sample its PID, then quits through the production
Quit action. After all twenty cycles, it reported zero GPUI windows but retained
144,475,168 bytes (137.8 MiB) physical footprint; CPU averaged 0.0001% over ten
seconds. The issue reproduces without native accessibility references. The exact
upstream object retaining the allocations has not been established; no unsafe
manual Metal/AppKit release or dependency source patch was attempted.

The zero-window tray and initial UI meet their measured targets, but repeated
window destruction fails the intended <50 MB idle memory budget. Task 1 is
incomplete. The spec explicitly requires design review at this gate, so the
implementation has not expanded into VPN/routing functionality.

## Approved reuse lifecycle and remeasurement

The user approved one retained Settings window with native hide/reveal on
2026-10-16. Close is intercepted without destruction. macOS uses NSWindow
orderOut; Windows has a native ShowWindow adapter which is not yet live-tested.

A new release probe first failed against removal behavior. It then found a
re-entrancy issue: global actions can run while GPUI temporarily borrows the active
window, so updating its handle inside that callback fails. Show/hide/quit now defer
until dispatch finishes. OS reopen is scheduled through AsyncApp.update so deferred
effects also flush when no window has been created yet.

The updated probe asserts one stable WindowId, native visible/hidden state, correct
Settings check state in the emitted menu, disabled unimplemented actions, duplicate
Open coalescing, and twenty hide/reveal cycles. No mocks or accessibility client are
needed for these assertions. A separate native UI test used the actual titlebar
close button twenty times; logs showed one creation and repeated reveals.

After twenty programmatic cycles, over 300.042 hidden idle seconds:

- CPU average: 0.0001% of one core.
- Peak physical footprint: 30,933,976 bytes (29.5 MiB).
- Peak RSS: 99,401,728 bytes (94.8 MiB).
- Same retained window; vmmap corroborates a bounded charged footprint.

After twenty actual titlebar-close/reveal cycles, over 10.043 hidden seconds:

- CPU average: 0.0001% of one core.
- Peak physical footprint: 34,751,448 bytes (33.1 MiB).
- Peak RSS: 101,597,184 bytes (96.9 MiB).

These pass the intended idle <50 MB charged-memory and <0.5% CPU gates on this Mac.
Memory is intentionally retained rather than claimed to be fully released. The
original destruction results above remain the record of the failed experiment.
The regression probe now uses reuse by default, holds 330 seconds for measurement,
and accepts `--hold-seconds 0` for a short assertion-only run. The previous
60-second/zero-window probe behavior belongs to commit 8db5cfe.

The initial fresh-context review found no additional Important/Critical defect;
it noted stale check state after OS reopen. The approved lifecycle now derives the
check state from native visibility and refreshes after show/hide on both entry
paths. The probe checks the emitted menu state; actual native checkmark pixels
remain a separate visual criterion. No second reviewer was used for this change.

## Verification and remaining scope

- Release build and the single-instance Rust test passed.
- Release clippy for workspace/all targets passed with warnings denied in app code.
- `cargo fmt --all -- --check`, shell launcher syntax and `git diff --check` passed.
- Existing CLI suite: twelve PASS, no live route mutations.
- Idle/initial-window measurements above are app-only; no helper is implemented.
- Checked-menu state readback, actual compositor latency, GPU power, minimum
  macOS version, and all Windows behavior remain unverified.
- The unsigned bundle is a local development artifact; no service installed,
  routes changed, credentials imported, or remote publication performed.

Full local logs and reports are kept under this plan's ignored
`.superpowers/sdd/2026-10-15-gpui-desktop/` workspace for continuation.

## Core config checkpoint

Version-1 JSON accepts normalized IPv4/CIDR and explicit ASCII hostnames. Eight
config tests pass, including a real child-process kill during staged replacement;
one ignored child fixture is invoked by that test. Existing bytes survive invalid
input, corrupt input remains visible, missing config defaults, and Unix output
permissions are 0600. Config/import input is bounded to 1 MiB; unique rules to 4096.

Staging uses an already-resolved tempfile dependency; file data is synced before
atomic replacement and the Unix parent directory is synced afterward. If that final
sync fails, the error explicitly says a validated replacement may already exist;
callers must reload before retrying. This verifies process-crash atomicity on the
current Mac, not power-loss behavior or Windows durability. No UI config editing
or automatic import of private local infrastructure has been enabled yet.

## Core session and route policy (Task 3)

Pure deterministic tests pass for duplicate Connect, stale session/revision/reply
callbacks, cancellation during apply, 30-second preparation and 10-second helper
deadlines, adopted VPN preservation, DNS failure and replacement rollback
confirmation. Helper sequence numbers are monotonic within a random UUID session.
A failed cleanup requires explicit retry; no reconnect loop is introduced.

The planner refuses defaults and /1, full-tunnel or unverifiable profiles, changed
public/endpoint paths, physical connected subnet overlaps, conflicting existing
routes, invalid destinations and more than 4096 expanded routes. Dual-stack DNS
answers emit an IPv6 bypass warning; AAAA-only destinations fail. Protocol v1
uses bounded strict typed JSON (1 MiB); authentication and live interface
attestation belong to the native helper, not this codec.

These checks do not establish live routing, VPN support, IPC authorization, or
rollback durability. The desktop prototype still has Connect disabled.

## Owned-route journal (Task 4)

Ten integration tests plus a write-fault matrix pass with an in-memory route
backend and private temporary files. The matrix reloads the journal after each
of four apply/clear writes, both a simulated crash after durable write and a
failed write before replacement. Separate faults interrupt the backend after
add/delete. This is process-level fault injection, not a power-loss or native
route test. The helper binary exits unsupported until its service exists.

The journal uses an OS-held lock, strict bounded JSON, same-directory atomic
replacement and file/directory synchronization on Unix. Unix storage requires
a private owner-only parent, regular single-link files owned by the effective
UID, and no final-component symlinks. The privileged installer must establish
the trusted parent and fixed path; caller-selected paths are not an IPC feature.
Windows ACL and metadata durability qualification remain unverified.

Pre-existing identical routes are never owned; foreign or changed routes are
never replaced/deleted. Known replacement failures attempt restoration of the
previous plan. Uncertain or failed work remains durable and blocks new apply.
A crash between add and durable ownership confirmation is intentionally not
automatically resolved when an identical route is present: external creation
is indistinguishable without a native ownership token. The route and journal
are retained for explicit administrator reconciliation. A committed plan alone
is not proof of live routing; native readback/authorization are still required.

## Helper request policy (Task 5, incomplete)

Pure tests exercise a trusted native peer identity supplied outside JSON, console
UID/code-requirement checks, one bound connection/session, monotonic sequences,
replay rejection, interface substitution, split-mode/path re-attestation on every
Apply, malformed frames and bound-connection invalidation cleanup. A durable
incomplete marker prevents partial replacements from claiming restored status.
These are policy tests with simulated native identity/snapshots, not proof of
authenticated XPC or live route safety. The helper executable still exits
unsupported. Native transport, profile attestation, route adapter, registration
and controlled integration tests remain incomplete. No signing identity was
available when checked; no privileged service or live route was changed.

Policy checkpoint verification: release workspace suite 44 active tests passed
(plus the ignored child fixture exercised by its parent); Clippy all targets
with warnings denied and formatting pass. Native helper Task5 remains incomplete.

## Native macOS helper implementation checkpoint

The native implementation now builds against the local macOS SDK: authenticated
NSXPC, ServiceManagement registration entry point, SystemConfiguration profile
and IPv4 snapshot reading, PF_ROUTE read/add/delete, fixed protected journal path
and session-only passive network subscriptions. An unprivileged anonymous XPC
probe accepted the running test binary's designated requirement and rejected a
wrong cdhash before the exported method. The route parser probe covers truncated
RTM messages, malformed masks and exact destination/gateway/interface identities.
Read-only native routing/profile enumeration passed; helper startup as a normal
user was denied before storage or mutation.

Production XPC requires Apple-anchored signatures for the same team and exact
app/helper identifiers. No signing identity is available, so no production
helper registration or route write was performed. Native routing writes, signed
IPC, administrative denial/retry, crash, reboot and real VPN/public/internal
connectivity remain UNVERIFIED. Task5's live qualification checkbox stays open.
The saved PPP VPN endpoint must currently be a literal IPv4 address. This
limitation prevents ambiguous protection of a DNS name's changing server set.
Journals record the macOS boot UUID; present matching routes from a prior boot
become uncertain rather than being claimed as still owned.
