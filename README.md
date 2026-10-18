# VPN splitter for macOS (L2TP)

Route office IPv4 traffic through an active L2TP VPN while keeping other IPv4
traffic on Wi-Fi or Ethernet. No package installation is required.

Connect your VPN, then prepare your local destination list:

```sh
cp config.example.txt config.txt
```

Replace the placeholder in `config.txt` with your office IPv4 addresses,
CIDRs, or hostnames, one per line. Full URLs and wildcard domains are not
supported. Hostnames must resolve successfully before routing is changed.

```sh
./vpn-splitter.sh status
DRY_RUN=1 ./vpn-splitter.sh apply
sudo -v
./vpn-splitter.sh apply
./vpn-splitter.sh clear
```

The script reads the physical gateway from macOS network service settings.
It preserves the VPN default route and adds two more specific internet routes
through Wi-Fi or Ethernet. More specific office routes use the VPN. Each IPv4
DNS server keeps its existing path; system DNS settings are not overwritten.
IPv6 routing is not changed.

Only routes created by the script are recorded in a private temporary state
directory. A failed `apply` triggers rollback. `clear` removes recorded routes,
keeps pre-existing routes, and continues when a recorded route has disappeared.
If cleanup fails, its remaining state is kept so you can retry `clear`.

Repeated `apply` is a no-op while active. After editing destinations,
reconnecting the VPN, or changing networks, run `clear` followed by `apply`.
Hostnames are resolved again during the new application. Newly created
subdomains are not automatically discovered.

`DRY_RUN=1` works for both `apply` and `clear`, without changing routes or state.

## Keep local data private

`config.txt`, infrastructure audit reports, credentials, logs, and local state
are excluded from Git. This repository contains a placeholder configuration
and tests using documentation address ranges. Review staged files before
publishing changes, and never force-add local configuration or audit reports.

## Tests

The tests mock operating system commands and do not change the Mac's routing:

```sh
bash test_vpn_splitter.sh
```

## GPUI desktop app (macOS preview)

Build the menu bar app on Apple Silicon with Xcode and Rust 1.97.1:

```sh
./scripts/package-macos.sh
open "target/release/VPN Splitter.app"
```

Open **VPN → Settings**, select a saved L2TP profile, enter or import destination
IPv4 addresses/CIDRs/hostnames, then Save. Disable **Send all traffic** in the
macOS VPN profile first. Configuration stays in your local Application Support
folder; the app does not import the CLI's private configuration automatically.

The UI includes Connect/Disconnect, routing status, cleanup retry and diagnostics.
**Automatic VPN/routing is not yet qualified for use.** It requires an authenticated
privileged helper and an app/helper signed by the same Apple developer team.
Unsigned local builds can edit configuration but cannot activate that helper.
No helper is installed by building or opening the app.

For a signed build, set `VPN_SPLITTER_SIGN_IDENTITY` in your local build environment
before running the packaging script. The script signs the embedded helper first,
then the app. Signing is not notarization or proof of live VPN integration.
Installation/admin approval and live route cleanup still require qualification on
a controlled Mac. Keep using the CLI above for routing until that is complete.

Windows support and further performance/accessibility qualification are deferred
while the macOS product is completed. See [desktop validation](docs/desktop-validation.md)
for measured results and incomplete checks.
