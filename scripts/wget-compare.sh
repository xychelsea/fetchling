#!/usr/bin/env bash
# Compare fetchling vs wget on a short localhost fixture list.
# Requires: wget, python3, a built fetchling binary (FETCHLING_BIN or target/release|debug).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -n "${FETCHLING_BIN:-}" ]]; then
  FL="$FETCHLING_BIN"
elif [[ -x "$ROOT/target/release/fetchling" ]]; then
  FL="$ROOT/target/release/fetchling"
elif [[ -x "$ROOT/target/debug/fetchling" ]]; then
  FL="$ROOT/target/debug/fetchling"
else
  echo "Building fetchling (debug)..."
  cargo build -p fetchling
  FL="$ROOT/target/debug/fetchling"
fi

command -v wget >/dev/null || { echo "wget is required" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/fetchling-wget-compare.XXXXXX")"
cleanup() { rm -rf "$WORKDIR"; }
trap cleanup EXIT

DOCROOT="$WORKDIR/www"
mkdir -p "$DOCROOT"
printf 'hello-compare\n' >"$DOCROOT/file.bin"
printf 'partial-prefix' >"$DOCROOT/resume.bin"

# Start static HTTP server
PORT_FILE="$WORKDIR/port"
python3 - "$DOCROOT" "$PORT_FILE" <<'PY' &
import http.server, socketserver, sys, pathlib
root = pathlib.Path(sys.argv[1])
port_file = pathlib.Path(sys.argv[2])

class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(root), **kwargs)
    def log_message(self, *args):
        pass

with socketserver.TCPServer(("127.0.0.1", 0), Handler) as httpd:
    port = httpd.server_address[1]
    port_file.write_text(str(port))
    httpd.serve_forever()
PY
SERVER_PID=$!
kill_server() { kill "$SERVER_PID" 2>/dev/null || true; }
trap 'kill_server; cleanup' EXIT

for _ in $(seq 1 50); do
  [[ -f "$PORT_FILE" ]] && break
  sleep 0.05
done
PORT="$(cat "$PORT_FILE")"
BASE="http://127.0.0.1:${PORT}"

fail=0
compare_bytes() {
  local name="$1" a="$2" b="$3"
  if ! cmp -s "$a" "$b"; then
    echo "FAIL [$name]: outputs differ"
    echo "  fetchling: $(wc -c <"$a") bytes"
    echo "  wget:      $(wc -c <"$b") bytes"
    fail=1
  else
    echo "ok   [$name]"
  fi
}

# 1) Plain GET
mkdir -p "$WORKDIR/fl1" "$WORKDIR/wg1"
"$FL" -q --tries=1 -O "$WORKDIR/fl1/file.bin" "$BASE/file.bin"
wget -q -O "$WORKDIR/wg1/file.bin" "$BASE/file.bin"
compare_bytes "plain-get" "$WORKDIR/fl1/file.bin" "$WORKDIR/wg1/file.bin"

# 2) Resume (-c): start with shared prefix, append remainder
mkdir -p "$WORKDIR/fl2" "$WORKDIR/wg2"
printf 'partial-prefix' >"$WORKDIR/fl2/resume.bin"
printf 'partial-prefix' >"$WORKDIR/wg2/resume.bin"
# Append rest on server file for full content
printf 'partial-prefixAND-REST\n' >"$DOCROOT/resume.bin"
"$FL" -q --tries=1 -c -O "$WORKDIR/fl2/resume.bin" "$BASE/resume.bin"
wget -q -c -O "$WORKDIR/wg2/resume.bin" "$BASE/resume.bin"
compare_bytes "continue" "$WORKDIR/fl2/resume.bin" "$WORKDIR/wg2/resume.bin"

# 3) stdout (-O -)
"$FL" -q --tries=1 -O - "$BASE/file.bin" >"$WORKDIR/fl-stdout.bin"
wget -q -O - "$BASE/file.bin" >"$WORKDIR/wg-stdout.bin"
compare_bytes "stdout" "$WORKDIR/fl-stdout.bin" "$WORKDIR/wg-stdout.bin"

# 4) Timestamping (-N): local file mtime matches server → both keep existing bytes (304).
mkdir -p "$WORKDIR/fl3" "$WORKDIR/wg3"
cp "$DOCROOT/file.bin" "$WORKDIR/fl3/file.bin"
cp "$DOCROOT/file.bin" "$WORKDIR/wg3/file.bin"
touch -r "$DOCROOT/file.bin" "$WORKDIR/fl3/file.bin" "$WORKDIR/wg3/file.bin"
"$FL" -q --tries=1 -N -O "$WORKDIR/fl3/file.bin" "$BASE/file.bin"
wget -q -N -O "$WORKDIR/wg3/file.bin" "$BASE/file.bin"
compare_bytes "timestamping-N" "$WORKDIR/fl3/file.bin" "$WORKDIR/wg3/file.bin"

if [[ "$fail" -ne 0 ]]; then
  echo "wget-compare: mismatches detected" >&2
  exit 1
fi
echo "wget-compare: all cases matched"
