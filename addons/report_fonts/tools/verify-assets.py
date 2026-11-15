# SPDX-License-Identifier: MIT OR Apache-2.0
"""Re-hash every saved font and notice; records current evidence, never prior times."""
import datetime
import hashlib
import json
import platform
import socket
import sys
from pathlib import Path

root = Path(__file__).resolve().parent.parent
inventory = json.loads((root / 'source-inventory.json').read_text())
manifest = json.loads((root / 'fonts.json').read_text())
records = []
seen = set()
for row in inventory:
    relative = Path(row['file'])
    if relative.is_absolute() or '..' in relative.parts or row['file'] in seen:
        raise ValueError('Unsafe or repeated inventory path')
    seen.add(row['file'])
    data = (root / relative).read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if len(data) != row['bytes'] or digest != row['sha256']:
        raise ValueError('Saved bytes disagree with inventory: ' + row['file'])
    if relative.suffix == '.ttf' and not 0 < len(data) <= 20 * 1024 * 1024:
        raise ValueError('Font exceeds size admission: ' + row['file'])
    if '/cc39c4c2c304bc368c9b7c311ea2bcc6013299fe/' not in row['source']:
        raise ValueError('Unexpected source revision')
    records.append(dict(file=row['file'], bytes=len(data), sha256=digest, source=row['source']))
font_rows = [{k: row[k] for k in ('family', 'file', 'sha256', 'source', 'license')} for row in inventory if row['file'].endswith('.ttf')]
if font_rows != manifest:
    raise ValueError('Font manifest disagrees with source inventory')
print(json.dumps(dict(command=['python3', 'addons/report_fonts/tools/verify-assets.py'],
    utc=datetime.datetime.now(datetime.timezone.utc).isoformat(), host=socket.gethostname(),
    platform=platform.platform(), python=sys.version, saved_files=records,
    result='All saved fonts/notices match their inventories; each font is within 20 MiB. Manifest rows match inventory.',
    scope='Current local byte verification only. No historic timestamps inferred; no parser, renderer or shaping tests run.'), indent=2))
