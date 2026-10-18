# Compiled into the desktop. Never execute a preview's mutable installer as root.
set -euo pipefail
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077
source_bundle=$1
expected_hash=$2
mode=$3
[[ "$expected_hash" =~ ^[0-9a-f]{40}$ ]] || exit 2
[[ "$mode" == install || "$mode" == remove ]] || exit 2
staging=$(mktemp -d /Library/PrivilegedHelperTools/.vpn-bootstrap.XXXXXX)
trap 'rm -rf "$staging"' EXIT
ditto --noacl --noextattr --noqtn "$source_bundle" "$staging/VPN Splitter.app"
[[ -z "$(find "$staging/VPN Splitter.app" -type l -print -quit)" ]] || exit 3
chmod -RN "$staging/VPN Splitter.app"
chown -R -P root:wheel "$staging/VPN Splitter.app"
chmod -R go-w "$staging/VPN Splitter.app"
codesign --verify --deep --strict -R "=identifier \"com.candratama.vpnsplitter\" and cdhash H\"$expected_hash\"" "$staging/VPN Splitter.app"
/bin/bash "$staging/VPN Splitter.app/Contents/Resources/install-local.sh" "$mode" "$source_bundle"
