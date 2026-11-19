#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Build the compiled client with the reporting addon and run web/scripts/browser-reporting.mjs (a Chrome and the Playwright
# install of the QA directory are needed; run it on a build host, never on the laptop):
#   CHROME_BIN=/path/to/chrome CLIENT_BROWSER_OUTPUT=$PWD/logs/reporting-ui web/scripts/browser-reporting.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
eval "$(scripts/web-toolchain.sh env)"
(cd web && pnpm install --frozen-lockfile > /dev/null)
scripts/build-gate.sh "browser-reporting" -- cargo build --locked -p erp-ui-compiler-cli --bin erp-ui-compile
target_dir="${CARGO_TARGET_DIR:-target}"
case "$target_dir" in /*) compiler="$target_dir/debug/erp-ui-compile" ;; *) compiler="$PWD/$target_dir/debug/erp-ui-compile" ;; esac
ERP_UI_COMPILER="$compiler" node web/scripts/build-human-ui.mjs --addons web,reporting
node web/scripts/browser-reporting.mjs
