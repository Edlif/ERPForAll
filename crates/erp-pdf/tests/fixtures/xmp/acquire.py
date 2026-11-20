# SPDX-License-Identifier: MIT OR Apache-2.0
"""Acquire unchanged Apache-2.0 fixture and notices at an immutable revision."""
import hashlib
import pathlib
import platform
import sys
import urllib.request
from datetime import datetime, timezone

ROOT = pathlib.Path(__file__).resolve().parent
PIN = "fff879ba82e4067915878b5368d8e7a0193a6ae5"
FILES = {
    "pdfbox-notepad-a1b.xmp": "xmpbox/src/test/resources/validxmp/Notepad++_A1b.xmp",
    "APACHE-LICENSE.txt": "LICENSE.txt",
    "APACHE-NOTICE.txt": "NOTICE.txt",
}
with (ROOT / "acquisition.log").open("w") as log:
    print(datetime.now(timezone.utc).isoformat(), platform.node(), sys.version, file=log)
    for name, path in FILES.items():
        url = f"https://raw.githubusercontent.com/apache/pdfbox/{PIN}/{path}"
        data = urllib.request.urlopen(url, timeout=30).read()
        (ROOT / name).write_bytes(data)
        print(url, hashlib.sha256(data).hexdigest(), name, file=log)
