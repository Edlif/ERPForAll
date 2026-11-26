# SPDX-License-Identifier: MIT OR Apache-2.0
"""Capture real predecessor XML/manifest bytes and tool-written source receipts. No build/test."""
import datetime
import hashlib
import json
from pathlib import Path
import platform
import sys
import subprocess
import tomllib

root = Path(__file__).resolve().parents[5]
out = Path(__file__).resolve().parent
# One receipt per capture run; a run captures only the receipts named on its command line (all
# without arguments), so an earlier receipt whose revisions this checkout lacks stays as written.
RECEIPTS = {
    "provenance.json": [
        "33eb050f195351aeac7f8030e2d73ff5bfc499ee",
        "339d5ee2d4",
        "d2716b3b972a64fd577c418ecbb2a6ddde21c9c9",
        "fe3f0d0de78e1c1659c192abcbeabb1ace786af8",
    ],
    "provenance-1.10-1.13.json": [
        "603a94965fcbde2ad4285bfeb196049b79aed1d8",
        "99de24dafd31eda9466146fe025d9580c509153e",
    ],
}
for receipt_name in sys.argv[1:] or list(RECEIPTS):
    sources = []
    for revision in RECEIPTS[receipt_name]:
        pin = subprocess.check_output(["git", "rev-parse", revision], cwd=root, text=True).strip()
        def source(name):
            return subprocess.check_output(["git", "show", f"{pin}:addons/account_reporting/{name}"], cwd=root)
        manifest = source("manifest.toml")
        metadata = tomllib.loads(manifest.decode())
        target = out / metadata["version"] / "account_reporting"
        files = {}
        for name in ["manifest.toml", *metadata["data"]]:
            data = source(name)
            path = target / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            files[name] = hashlib.sha256(data).hexdigest()
        sources.append({"revision": pin, "version": metadata["version"], "files_sha256": files})
    receipt = {"captured_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
               "host_sha256": hashlib.sha256(platform.node().encode()).hexdigest(), "python": platform.python_version(),
               "git": subprocess.check_output(["git", "--version"], text=True).strip(),
               "command": " ".join(["python3 -I crates/erp-cli/tests/fixtures/account_reporting_previous/capture.py", *sys.argv[1:]]),
               "sources": sources}
    (out / receipt_name).write_text(json.dumps(receipt, indent=2) + "\n")
