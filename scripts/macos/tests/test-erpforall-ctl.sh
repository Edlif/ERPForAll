#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Tests of erpforall-ctl against a fake checkout and a fake server, so nothing is built and no database starts.
# Needs bash, python3 and curl. Written for Linux; on macOS `ps -o comm=` names the interpreter of a script,
# so the fake server is not recognised there.
#   scripts/macos/tests/test-erpforall-ctl.sh
set -euo pipefail

CTL="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/erpforall-ctl"
WORK="$(mktemp -d)"
FAKE_ROOT="$WORK/checkout"
STATE="$WORK/state"
BUSY_PID=""
cleanup() {
  if [ -f "$STATE/server.pid" ]; then kill "$(cat "$STATE/server.pid")" 2>/dev/null || true; fi
  if [ -n "$BUSY_PID" ]; then kill "$BUSY_PID" 2>/dev/null || true; fi
  rm -rf "$WORK"
}
trap cleanup EXIT

PASS=0
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
ok() { PASS=$((PASS + 1)); printf 'ok %s - %s\n' "$PASS" "$*"; }
ctl() { ERPFORALL_ROOT="$FAKE_ROOT" ERPFORALL_LOCAL_STATE="$STATE" PGUSER=test "$CTL" "$@"; }
expect_status() { local got; got="$(ctl status)"; [ "$got" = "$1" ] || fail "status: expected $1, got $got"; }
free_port() { python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }

PORT="$(free_port)"
mkdir -p "$FAKE_ROOT/scripts" "$FAKE_ROOT/target" "$STATE/private" "$WORK/bin"

# A fake server named erp: answers 200 on /erp/health/ready and exits on SIGTERM.
cat > "$WORK/bin/erp" <<'EOF'
#!/bin/bash
python3 -I -c '
import http.server, sys
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == "/erp/health/ready" else 404); self.end_headers()
    def log_message(self, *a): pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
' "$1" &
child=$!
trap 'kill "$child"; exit 0' TERM
wait "$child"
EOF
chmod +x "$WORK/bin/erp"

# A fake install.sh: records its flags and, for an installed state, starts the fake server like install.sh does.
cat > "$FAKE_ROOT/scripts/install.sh" <<EOF
#!/bin/bash
set -eu
printf '%s\n' "\$*" >> "$WORK/install.calls"
nohup "$WORK/bin/erp" "$PORT" > /dev/null 2>&1 < /dev/null &
echo "\$!" > "$STATE/server.pid"
for _ in \$(seq 50); do
  [ "\$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:$PORT/erp/health/ready)" = 200 ] && exit 0
  sleep 0.1
done
exit 1
EOF
chmod +x "$FAKE_ROOT/scripts/install.sh"
last_call() { tail -n 1 "$WORK/install.calls"; }

# 1. Nothing installed.
expect_status not-installed
ok "status is not-installed without a state file"
if ctl start 2>"$WORK/err"; then fail "start succeeded without an installation"; fi
grep -q 'not installed' "$WORK/err" || fail "start without installation: $(cat "$WORK/err")"
ok "start refuses when not installed"

# 2. Installed, no binary.
printf 'DB_NAME=erpforall_local\nPORT=%s\nPG_MODE=local\nCONTAINER=erpforall-local-postgres\nDATABASE_PHASE=done\n' "$PORT" > "$STATE/state.env"
printf 's3cret-admin-password\n' > "$STATE/private/admin-password"
expect_status stopped
ok "status is stopped for an installed state without a server"
if ctl start 2>"$WORK/err"; then fail "start succeeded without an erp binary"; fi
grep -q 'no erp binary' "$WORK/err" || fail "start without binary: $(cat "$WORK/err")"
ok "start refuses without an erp binary"

# 3. Release binary: start with no flags, then running.
mkdir -p "$FAKE_ROOT/target/release" "$FAKE_ROOT/target/local"
touch "$FAKE_ROOT/target/release/erp"; chmod +x "$FAKE_ROOT/target/release/erp"
ctl start > /dev/null
[ "$(last_call)" = "" ] || fail "start with a release binary passed flags: '$(last_call)'"
expect_status running
[ "$(ctl url)" = "http://127.0.0.1:$PORT/erp" ] || fail "url: $(ctl url)"
ok "start runs install.sh without flags for a release build; status running; url"
[ "$(ctl start)" = "already running: http://127.0.0.1:$PORT/erp" ] || fail "second start did not say already running"
ok "start is a no-op while running"

# 4. Stop.
ctl stop > "$WORK/out"
grep -q 'server stopped' "$WORK/out" || fail "stop: $(cat "$WORK/out")"
expect_status stopped
ok "stop ends the server"
ctl stop | grep -q 'server was not running' || fail "second stop"
ok "stop is a no-op when stopped"
if ctl stop --bogus 2>/dev/null; then fail "stop accepted an unknown option"; fi
ok "stop refuses an unknown option"

# 5. A newer local-profile binary selects --fast.
sleep 1; touch "$FAKE_ROOT/target/local/erp"; chmod +x "$FAKE_ROOT/target/local/erp"
ctl start > /dev/null
[ "$(last_call)" = "--fast" ] || fail "start with a newer local binary: '$(last_call)'"
ctl stop > /dev/null
ok "start passes --fast when the local build is the newer one"
ctl rebuild > /dev/null
[ "$(last_call)" = "--rebuild --fast" ] || fail "rebuild: '$(last_call)'"
ctl stop > /dev/null
ok "rebuild passes --rebuild with the profile"

# 6. Port taken by another program.
python3 -I -m http.server --bind 127.0.0.1 "$PORT" > /dev/null 2>&1 &
BUSY_PID=$!
for _ in $(seq 50); do (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null && break; sleep 0.1; done
expect_status port-busy
if ctl start 2>"$WORK/err"; then fail "start succeeded on a busy port"; fi
grep -q 'used by another program' "$WORK/err" || fail "busy port: $(cat "$WORK/err")"
kill "$BUSY_PID"; wait "$BUSY_PID" 2>/dev/null || true; BUSY_PID=""
ok "status port-busy and start refuses when another program holds the port"

# 7. The password is never printed.
[ "$(ctl password-file)" = "$STATE/private/admin-password" ] || fail "password-file: $(ctl password-file)"
if ctl info | grep -q 's3cret'; then fail "info printed the password"; fi
ok "password-file and info print the path, never the password"

# 8. Logs.
printf 'line1\nline2\nline3\n' > "$STATE/server.log"
[ "$(ctl logs 2)" = "$(printf 'line2\nline3')" ] || fail "logs 2"
if ctl logs abc 2>/dev/null; then fail "logs accepted a non-number"; fi
ok "logs prints the tail and refuses a non-number"

printf 'all %s tests passed\n' "$PASS"
