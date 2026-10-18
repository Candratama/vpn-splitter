#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo 'Requires macOS.' >&2; exit 1; }
cargo build --release --locked -p vpn-splitter-desktop -p vpn-splitter-helper
python3 - <<'PY'
from pathlib import Path
import plistlib
import shutil
import tempfile

output = Path('target/release/VPN Splitter.app')
with tempfile.TemporaryDirectory(dir=output.parent, prefix='vpn-bundle-') as staging:
    bundle = Path(staging) / 'VPN Splitter.app'
    contents = bundle / 'Contents'
    (contents / 'MacOS').mkdir(parents=True)
    (contents / 'Library/LaunchDaemons').mkdir(parents=True)
    for name in ('vpn-splitter-desktop', 'vpn-splitter-helper'):
        shutil.copy2(Path('target/release') / name, contents / 'MacOS' / name)
    shutil.copy2('packaging/macos/com.candratama.vpnsplitter.helper.plist',
                 contents / 'Library/LaunchDaemons/com.candratama.vpnsplitter.helper.plist')
    (contents / 'Info.plist').write_bytes(plistlib.dumps({
        'CFBundleIdentifier': 'com.candratama.vpnsplitter',
        'CFBundleName': 'VPN Splitter',
        'CFBundleDisplayName': 'VPN Splitter',
        'CFBundleExecutable': 'vpn-splitter-desktop',
        'CFBundlePackageType': 'APPL',
        'CFBundleVersion': '1',
        'CFBundleShortVersionString': '0.1.0',
        'LSUIElement': True,
        'LSMinimumSystemVersion': '14.0',
        'NSHighResolutionCapable': True,
    }))
    if output.exists():
        shutil.rmtree(output)
    shutil.move(str(bundle), output)
PY
bundle='target/release/VPN Splitter.app'
if [[ -n "${VPN_SPLITTER_SIGN_IDENTITY:-}" ]]; then
    codesign --force --options runtime --timestamp --sign "$VPN_SPLITTER_SIGN_IDENTITY" \
        --identifier com.candratama.vpnsplitter.helper "$bundle/Contents/MacOS/vpn-splitter-helper"
    codesign --force --options runtime --timestamp --sign "$VPN_SPLITTER_SIGN_IDENTITY" "$bundle"
    codesign --verify --deep --strict "$bundle"
    echo "Signed local bundle: $bundle (not notarized; integration qualification required)."
else
    echo "Unsigned local bundle: $bundle"
    echo 'Settings works; VPN routing requires app/helper signed by the same Apple developer team.'
fi
