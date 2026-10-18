#!/bin/bash
# Called only after macOS administrator approval. No credentials are stored.
set -euo pipefail
PATH=/usr/bin:/bin:/usr/sbin:/sbin
export PATH
umask 077
[[ "$(id -u)" == 0 ]] || { echo 'Administrator approval is required.' >&2; exit 1; }
mode=${1:-install}
[[ "$mode" == install || "$mode" == remove ]] || exit 2
label=com.candratama.vpnsplitter.helper
installed='/Library/PrivilegedHelperTools/VPN Splitter.app'
plist="/Library/LaunchDaemons/$label.plist"
source_bundle=$(cd "$(dirname "$0")/../.." && pwd -P)
origin_bundle=${2:-$source_bundle}

# Never follow a user-created symlink at a privileged destination.
for path in /Library /Library/PrivilegedHelperTools /Library/LaunchDaemons; do
    [[ -d "$path" && ! -L "$path" && "$(stat -f %u "$path")" == 0 ]] || exit 3
    permissions=$(stat -f %OLp "$path")
    (( (8#$permissions & 8#022) == 0 )) || exit 3
done
[[ ! -L "$installed" && ! -L "$plist" ]] || exit 3
if [[ -e "$installed" ]]; then
    [[ -d "$installed" && "$(stat -f %u "$installed")" == 0 ]] || exit 3
fi
if [[ -e "$plist" ]]; then
    [[ -f "$plist" && "$(stat -f %u "$plist")" == 0 ]] || exit 3
fi

if [[ "$mode" == remove ]]; then
    [[ "$origin_bundle" == "$installed" ]] || { echo 'Remove from the installed app.' >&2; exit 4; }
    # Unload first, then reconcile durable ownership. A failed cleanup retains
    # the app, daemon plist and journal so the user can retry safely.
    if launchctl print "system/$label" >/dev/null 2>&1; then
        launchctl bootout "system/$label"
    fi
    if ! "$installed/Contents/MacOS/vpn-splitter-helper" --cleanup; then
        launchctl bootstrap system "$plist" || true
        echo 'Cleanup incomplete. Installation and recovery journal retained.' >&2
        exit 5
    fi
    rm -f "$plist"
    echo 'Helper removed. App and recovery journal retained; no VPN profile or user config deleted.'
    exit 0
fi

staging=$(mktemp -d /Library/PrivilegedHelperTools/.vpn-splitter.XXXXXX)
trap 'rm -rf "$staging"' EXIT

if [[ "$origin_bundle" != "$installed" ]]; then
    # Updating an installed pair requires removing the helper first. Preserve
    # the existing app and its hashes while a service might still be active.
    if ps -axo comm= | grep -Fx "$installed/Contents/MacOS/vpn-splitter-desktop" >/dev/null; then
        echo 'Quit the installed app before updating.' >&2
        exit 6
    fi
    if [[ -e "$installed" ]]; then
        codesign --verify --deep --strict "$installed"
        if launchctl print "system/$label" >/dev/null 2>&1; then
            launchctl bootout "system/$label"
        fi
        if ! "$installed/Contents/MacOS/vpn-splitter-helper" --cleanup; then
            if [[ -f "$plist" ]]; then launchctl bootstrap system "$plist" || true; fi
            echo 'Update stopped: cleanup incomplete; previous installation retained.' >&2
            exit 5
        fi
        rm -f "$plist"
    fi
    ditto --noacl --noextattr --noqtn "$source_bundle" "$staging/VPN Splitter.app"
    [[ -z "$(find "$staging/VPN Splitter.app" -type l -print -quit)" ]] || exit 3
    chmod -RN "$staging/VPN Splitter.app"
    chown -R -P root:wheel "$staging/VPN Splitter.app"
    chmod -R go-w "$staging/VPN Splitter.app"
    codesign --verify --deep --strict "$staging/VPN Splitter.app"
    for pair in 'vpn-splitter-desktop:com.candratama.vpnsplitter' 'vpn-splitter-helper:com.candratama.vpnsplitter.helper'; do
        executable=${pair%%:*}
        identifier=${pair#*:}
        codesign --verify -R "=identifier \"$identifier\"" "$staging/VPN Splitter.app/Contents/MacOS/$executable"
    done
    if [[ -e "$installed" ]]; then
        mv "$installed" "$staging/previous.app"
    fi
    if ! mv "$staging/VPN Splitter.app" "$installed"; then
        if [[ -e "$staging/previous.app" ]]; then mv "$staging/previous.app" "$installed"; fi
        exit 7
    fi
fi
codesign --verify --deep --strict "$installed"
if [[ ! -e "$plist" ]]; then
    /usr/libexec/PlistBuddy -c "Add :Label string $label" "$staging/helper.plist"
    /usr/libexec/PlistBuddy -c 'Add :ProgramArguments array' "$staging/helper.plist"
    /usr/libexec/PlistBuddy -c "Add :ProgramArguments:0 string $installed/Contents/MacOS/vpn-splitter-helper" "$staging/helper.plist"
    /usr/libexec/PlistBuddy -c 'Add :MachServices dict' "$staging/helper.plist"
    /usr/libexec/PlistBuddy -c "Add :MachServices:$label bool true" "$staging/helper.plist"
    /usr/libexec/PlistBuddy -c 'Add :ProcessType string Interactive' "$staging/helper.plist"
    chown root:wheel "$staging/helper.plist"
    chmod 644 "$staging/helper.plist"
    mv "$staging/helper.plist" "$plist"
fi
if ! launchctl print "system/$label" >/dev/null 2>&1; then
    launchctl bootstrap system "$plist"
fi
echo 'Local helper registered. Quit the preview and open /Library/PrivilegedHelperTools/VPN Splitter.app.'
