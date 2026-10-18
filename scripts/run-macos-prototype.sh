#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
[[ "$(uname -s)" == Darwin ]] || { echo 'Requires macOS.' >&2; exit 1; }
cargo build --release --locked -p vpn-splitter-desktop
python3 - <<'PY'
from pathlib import Path
import plistlib
import shutil
import tempfile

bundle = Path('target/release/VPN Splitter Prototype.app/Contents')
(bundle / 'MacOS').mkdir(parents=True, exist_ok=True)
with tempfile.NamedTemporaryFile(dir=bundle / 'MacOS', delete=False) as temporary:
    executable = Path(temporary.name)
try:
    shutil.copy2('target/release/vpn-splitter-desktop', executable)
    executable.replace(bundle / 'MacOS/vpn-splitter-desktop')
finally:
    executable.unlink(missing_ok=True)
(bundle / 'Info.plist').write_bytes(plistlib.dumps({
    'CFBundleIdentifier': 'com.candratama.vpnsplitter.prototype',
    'CFBundleName': 'VPN Splitter Prototype',
    'CFBundleDisplayName': 'VPN Splitter Prototype',
    'CFBundleExecutable': 'vpn-splitter-desktop',
    'CFBundlePackageType': 'APPL',
    'CFBundleVersion': '1',
    'CFBundleShortVersionString': '0.1.0',
    'LSUIElement': True,
    'LSMinimumSystemVersion': '14.0',
    'NSHighResolutionCapable': True,
}))
PY
exec './target/release/VPN Splitter Prototype.app/Contents/MacOS/vpn-splitter-desktop'
