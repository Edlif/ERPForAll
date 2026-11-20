#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Run only on a permitted build host, from this fixture directory.
set -euo pipefail
out=$(mktemp -d /tmp/erpforall-xmp-producer.XXXXXX)
trap 'rm -rf "$out"' EXIT
{
    date -u +%FT%TZ
    hostname
    uname -a
    libreoffice --version
    pdfinfo -v 2>&1
    python3 --version
    mkdir -p "$out/profile/user"
    cat > "$out/profile/user/registrymodifications.xcu" <<'XCU'
<?xml version="1.0" encoding="UTF-8"?>
<oor:items xmlns:oor="http://openoffice.org/2001/registry"><item oor:path="/org.openoffice.Office.Common/Filter/PDF/Export"><prop oor:name="SelectPdfVersion" oor:op="fuse"><value>3</value></prop><prop oor:name="PDFUACompliance" oor:op="fuse"><value>true</value></prop></item></oor:items>
XCU
    set -x
    libreoffice "-env:UserInstallation=file://$out/profile" --headless --convert-to pdf:writer_pdf_Export --outdir "$out" source.fodt
    pdfinfo -meta "$out/source.pdf" > "$out/extracted.txt"
    # pdfinfo adds one LF after the exact metadata stream. Retain packet only.
    python3 - "$out/extracted.txt" libreoffice-pdfa3.xmp "$out/source.pdf" <<'PY'
import pathlib, sys
raw = pathlib.Path(sys.argv[1]).read_bytes()
assert raw.endswith(b'<?xpacket end="w"?>\n\n'), repr(raw[-100:])
assert raw[:-1] in pathlib.Path(sys.argv[3]).read_bytes(), 'extraction must be exact PDF bytes'
pathlib.Path(sys.argv[2]).write_bytes(raw[:-1])
PY
    sha256sum source.fodt "$out/source.pdf" libreoffice-pdfa3.xmp
} > generation.log 2>&1
