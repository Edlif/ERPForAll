#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
set -euo pipefail
source_dir=$(cd "$(dirname "$0")" && pwd)
out=$(pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
cd "$scratch"
fetch() {
    local path=$1 digest=$2 jar=${1##*/}
    curl -fsS "https://repo.maven.apache.org/maven2/$path" -o "$jar"
    echo "$digest  $jar" | sha256sum -c -
}
fetch org/apache/pdfbox/pdfbox-app/3.0.4/pdfbox-app-3.0.4.jar fba2f689270a75ce730c080ee9070ea6b0a4d16b544bf436f06ccceab5522143
fetch org/apache/pdfbox/xmpbox/3.0.4/xmpbox-3.0.4.jar d0af3fd7222fb3e18372a28ef7a98595a3f7d45e0cf5ad30f917ad8612c1ddbb
fetch com/adobe/xmp/xmpcore/6.1.11/xmpcore-6.1.11.jar 8f7033c579b99fa0d9d6ddcb9448875b5e4b577c350002278ce46997d678b737
date -u +%FT%TZ
hostname
java -version
sha256sum "$source_dir/Generate.java"
java -cp 'pdfbox-app-3.0.4.jar:xmpbox-3.0.4.jar:xmpcore-6.1.11.jar' "$source_dir/Generate.java"
for file in empty qualified-and-nested; do
    sha256sum "$file.pdf" "$file.xmp"
    cp "$file.pdf" "$out/"
done
