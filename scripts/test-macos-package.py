#!/usr/bin/env python3
"""Verify a local bundle's hardened signatures and resource/hash pinning; no install."""
import plistlib
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

bundle = Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/VPN Splitter.app').resolve()


def verify(path, requirement=None):
    args = ['codesign', '--verify', '--deep', '--strict']
    if requirement:
        args += ['-R', '=' + requirement]
    return subprocess.run(args + [str(path)], capture_output=True).returncode == 0


assert verify(bundle), 'Invalid bundle signature'
info = plistlib.loads((bundle / 'Contents/Info.plist').read_bytes())
assert info['CFBundleIdentifier'] == 'com.candratama.vpnsplitter'
for executable in ['vpn-splitter-desktop', 'vpn-splitter-helper']:
    details = subprocess.run(['codesign', '-d', '--verbose=4', str(bundle / 'Contents/MacOS' / executable)], capture_output=True, text=True, check=True).stderr
    assert 'runtime' in details, f'{executable}: hardened runtime missing'
details = subprocess.run(['codesign', '-d', '--verbose=4', str(bundle)], capture_output=True, text=True, check=True).stderr
cdhash = re.search(r'^CDHash=([0-9a-f]{40})$', details, re.MULTILINE).group(1)
requirement = f'identifier "com.candratama.vpnsplitter" and cdhash H"{cdhash}"'
assert verify(bundle, requirement), 'Pinned main hash rejected'
with tempfile.TemporaryDirectory(prefix='vpn-package-check-') as temporary:
    copied = Path(temporary) / 'VPN Splitter.app'
    shutil.copytree(bundle, copied)
    assert verify(copied, requirement), 'Valid snapshot rejected'
    installer = copied / 'Contents/Resources/install-local.sh'
    installer.write_bytes(installer.read_bytes() + b'\n# changed after approval\n')
    assert not verify(copied, requirement), 'Modified installer accepted'
    shutil.copy2(bundle / 'Contents/Resources/install-local.sh', installer)
    helper = copied / 'Contents/MacOS/vpn-splitter-helper'
    helper.write_bytes(helper.read_bytes() + b'changed')
    assert not verify(copied, requirement), 'Modified helper accepted'
print('PASS: hardened pair, pinned snapshot, installer tamper rejection, helper tamper rejection')
