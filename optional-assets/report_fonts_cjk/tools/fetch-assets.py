# SPDX-License-Identifier: MIT OR Apache-2.0
"""Download the files of source-inventory.json that are absent, from their pinned URLs.

For a copy of this package that came without the font files. A file is written only when its
downloaded bytes have the byte count and SHA-256 of the inventory; nothing else is accepted, and
a file that is already present is left alone (verify it with tools/verify-assets.py).

    python3 optional-assets/report_fonts_cjk/tools/fetch-assets.py [--dest DIRECTORY]
"""
import argparse
import datetime
import hashlib
import json
import os
import platform
import socket
import sys
import urllib.request
from pathlib import Path

REVISION = 'cc39c4c2c304bc368c9b7c311ea2bcc6013299fe'
PREFIX = 'https://raw.githubusercontent.com/google/fonts/' + REVISION + '/ofl/'
FONT_BYTES = 20 * 1024 * 1024
TIMEOUT_SECONDS = 300

root = Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
parser.add_argument('--dest', type=Path, default=root, help='directory that receives the files (default: this package)')
args = parser.parse_args()
dest = args.dest.resolve()
if not dest.is_dir():
    raise SystemExit('fetch-assets: destination is not a directory: ' + str(dest))

inventory = json.loads((root / 'source-inventory.json').read_text())
fetched, present = [], []
for row in inventory:
    relative = Path(row['file'])
    if relative.is_absolute() or '..' in relative.parts:
        raise SystemExit('fetch-assets: unsafe inventory path: ' + row['file'])
    if not row['source'].startswith(PREFIX):
        raise SystemExit('fetch-assets: source is not the pinned revision: ' + row['source'])
    if not 0 < row['bytes'] <= FONT_BYTES:
        raise SystemExit('fetch-assets: inventory size is outside 1..20 MiB: ' + row['file'])
    target = dest / relative
    if target.exists():
        present.append(row['file'])
        continue
    with urllib.request.urlopen(row['source'], timeout=TIMEOUT_SECONDS) as response:
        data = response.read(row['bytes'] + 1)
    digest = hashlib.sha256(data).hexdigest()
    if len(data) != row['bytes'] or digest != row['sha256']:
        raise SystemExit('fetch-assets: downloaded bytes disagree with the inventory: ' + row['file']
                         + ' (' + str(len(data)) + ' bytes, sha256 ' + digest + ')')
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_name(target.name + '.partial')
    partial.write_bytes(data)
    os.replace(partial, target)
    fetched.append(dict(file=row['file'], bytes=len(data), sha256=digest, source=row['source']))
print(json.dumps(dict(command=['python3', 'optional-assets/report_fonts_cjk/tools/fetch-assets.py'] + sys.argv[1:],
    utc=datetime.datetime.now(datetime.timezone.utc).isoformat(), host=socket.gethostname(),
    platform=platform.platform(), python=sys.version, revision=REVISION, fetched=fetched, already_present=present,
    validation='Each fetched file has the byte count and SHA-256 of source-inventory.json. Files already present were not read.'), indent=2))
