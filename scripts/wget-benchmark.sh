#!/usr/bin/env bash
# Time fetchling vs wget on localhost fixtures (startup, throughput, concurrency).
# Requires: wget, python3, a built fetchling binary (FETCHLING_BIN or release under
# CARGO_TARGET_DIR / target/). Builds release fetchling if none is found.
#
# Usage:
#   ./scripts/wget-benchmark.sh
#
# Optional environment:
#   FETCHLING_BIN   Path to fetchling binary
#   SMALL_RUNS      Timed small-file runs (default: 20)
#   LARGE_RUNS      Timed large-file runs (default: 3)
#   MULTI_RUNS      Timed multi-URL runs (default: 3)
#   LARGE_MIB       Large fixture size in MiB (default: 256)
#   MULTI_PARTS     Number of multi-URL parts (default: 16)
#   MULTI_PART_MIB  Size of each multi-URL part in MiB (default: 4)
#   MAX_THREADS      fetchling --max-threads for multi (default: 8)
#   PER_HOST_THREADS fetchling --max-threads-per-host for multi
#                    (default: min(MAX_THREADS, 4), matching the CLI default)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

resolve_bin() {
  local cand
  if [[ -n "${FETCHLING_BIN:-}" ]]; then
    echo "$FETCHLING_BIN"
    return
  fi
  for cand in \
    "${CARGO_TARGET_DIR:+$CARGO_TARGET_DIR/release/fetchling}" \
    "$ROOT/target/release/fetchling" \
    "${CARGO_TARGET_DIR:+$CARGO_TARGET_DIR/debug/fetchling}" \
    "$ROOT/target/debug/fetchling"
  do
    [[ -n "$cand" && -x "$cand" ]] || continue
    echo "$cand"
    return
  done
  return 1
}

if FL="$(resolve_bin)"; then
  :
else
  echo "Building fetchling (release)..."
  cargo build --release -p fetchling
  FL="$(resolve_bin)" || {
    echo "fetchling binary not found after build" >&2
    exit 1
  }
fi

command -v wget >/dev/null || { echo "wget is required" >&2; exit 1; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 1; }

SMALL_RUNS="${SMALL_RUNS:-20}"
LARGE_RUNS="${LARGE_RUNS:-3}"
MULTI_RUNS="${MULTI_RUNS:-3}"
LARGE_MIB="${LARGE_MIB:-256}"
MULTI_PARTS="${MULTI_PARTS:-16}"
MULTI_PART_MIB="${MULTI_PART_MIB:-4}"
MAX_THREADS="${MAX_THREADS:-8}"
if [[ -z "${PER_HOST_THREADS:-}" ]]; then
  if (( MAX_THREADS < 4 )); then
    PER_HOST_THREADS="$MAX_THREADS"
  else
    PER_HOST_THREADS=4
  fi
fi
MULTI_TOTAL_MIB=$((MULTI_PARTS * MULTI_PART_MIB))

WORKDIR="$(mktemp -d "${TMPDIR:-/tmp}/fetchling-wget-bench.XXXXXX")"
DOCROOT="$WORKDIR/www"
OUT="$WORKDIR/out"
mkdir -p "$DOCROOT" "$OUT"

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

class ThreadingHTTPServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    daemon_threads = True

with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as httpd:
    port = httpd.server_address[1]
    port_file.write_text(str(port))
    httpd.serve_forever()
PY
SERVER_PID=$!
cleanup() { kill "$SERVER_PID" 2>/dev/null || true; rm -rf "$WORKDIR"; }
trap cleanup EXIT

echo "Generating fixtures (large=${LARGE_MIB}MiB, multi=${MULTI_PARTS}x${MULTI_PART_MIB}MiB)..."
dd if=/dev/urandom of="$DOCROOT/small.bin" bs=1024 count=1 status=none
dd if=/dev/urandom of="$DOCROOT/large.bin" bs=1M count="$LARGE_MIB" status=none
for i in $(seq -w 1 "$MULTI_PARTS"); do
  dd if=/dev/urandom of="$DOCROOT/part${i}.bin" bs=1M count="$MULTI_PART_MIB" status=none
done

for _ in $(seq 1 50); do
  [[ -f "$PORT_FILE" ]] && break
  sleep 0.05
done
[[ -f "$PORT_FILE" ]] || { echo "HTTP server failed to start" >&2; exit 1; }
PORT="$(cat "$PORT_FILE")"
BASE="http://127.0.0.1:${PORT}"

URLS="$WORKDIR/urls.txt"
: >"$URLS"
for i in $(seq -w 1 "$MULTI_PARTS"); do
  echo "$BASE/part${i}.bin" >>"$URLS"
done

mean() {
  awk '{s+=$1;n++} END{if(n) printf "%.6f", s/n; else print "nan"}'
}

elapsed() {
  local start="$1" end
  end=$(date +%s.%N)
  awk -v s="$start" -v e="$end" 'BEGIN{printf "%.6f\n", e-s}'
}

time_cmd() {
  local start
  start=$(date +%s.%N)
  "$@" >/dev/null 2>&1
  elapsed "$start"
}

run_fl_multi() {
  local d="$1"
  rm -rf "$d"
  mkdir -p "$d"
  (cd "$d" && "$FL" -q --tries=1 --max-threads "$MAX_THREADS" \
    --max-threads-per-host "$PER_HOST_THREADS" -i "$URLS")
}

run_wg_multi() {
  local d="$1"
  rm -rf "$d"
  mkdir -p "$d"
  while read -r u; do
    wget -q -O "$d/$(basename "$u")" "$u"
  done <"$URLS"
}

echo "fetchling: $FL"
echo "wget:      $(command -v wget) ($(wget --version | head -1))"
echo "base:      $BASE"
echo

# --- small ---
echo "=== small (1 KiB), ${SMALL_RUNS} timed runs after warmup ==="
"$FL" -q --tries=1 -O "$OUT/fl-small.bin" "$BASE/small.bin"
wget -q -O "$OUT/wg-small.bin" "$BASE/small.bin"
SMALL_FL_TIMES=()
SMALL_WG_TIMES=()
for _ in $(seq 1 "$SMALL_RUNS"); do
  SMALL_FL_TIMES+=("$(time_cmd "$FL" -q --tries=1 -O "$OUT/fl-small.bin" "$BASE/small.bin")")
  SMALL_WG_TIMES+=("$(time_cmd wget -q -O "$OUT/wg-small.bin" "$BASE/small.bin")")
done
SMALL_FL_MEAN=$(printf '%s\n' "${SMALL_FL_TIMES[@]}" | mean)
SMALL_WG_MEAN=$(printf '%s\n' "${SMALL_WG_TIMES[@]}" | mean)
cmp -s "$OUT/fl-small.bin" "$DOCROOT/small.bin" && SMALL_FL_OK=ok || SMALL_FL_OK=FAIL
cmp -s "$OUT/wg-small.bin" "$DOCROOT/small.bin" && SMALL_WG_OK=ok || SMALL_WG_OK=FAIL
echo "fetchling mean=${SMALL_FL_MEAN}s integrity=$SMALL_FL_OK"
echo "wget     mean=${SMALL_WG_MEAN}s integrity=$SMALL_WG_OK"
echo

# --- large ---
echo "=== large (${LARGE_MIB} MiB), ${LARGE_RUNS} timed runs after warmup ==="
"$FL" -q --tries=1 -O "$OUT/fl-large.bin" "$BASE/large.bin"
wget -q -O "$OUT/wg-large.bin" "$BASE/large.bin"
LARGE_FL_TIMES=()
LARGE_WG_TIMES=()
for _ in $(seq 1 "$LARGE_RUNS"); do
  LARGE_FL_TIMES+=("$(time_cmd "$FL" -q --tries=1 -O "$OUT/fl-large.bin" "$BASE/large.bin")")
  LARGE_WG_TIMES+=("$(time_cmd wget -q -O "$OUT/wg-large.bin" "$BASE/large.bin")")
done
LARGE_FL_MEAN=$(printf '%s\n' "${LARGE_FL_TIMES[@]}" | mean)
LARGE_WG_MEAN=$(printf '%s\n' "${LARGE_WG_TIMES[@]}" | mean)
cmp -s "$OUT/fl-large.bin" "$DOCROOT/large.bin" && LARGE_FL_OK=ok || LARGE_FL_OK=FAIL
cmp -s "$OUT/wg-large.bin" "$DOCROOT/large.bin" && LARGE_WG_OK=ok || LARGE_WG_OK=FAIL
LARGE_FL_MIB_S=$(awk -v t="$LARGE_FL_MEAN" -v m="$LARGE_MIB" 'BEGIN{printf "%.2f", m/t}')
LARGE_WG_MIB_S=$(awk -v t="$LARGE_WG_MEAN" -v m="$LARGE_MIB" 'BEGIN{printf "%.2f", m/t}')
echo "fetchling mean=${LARGE_FL_MEAN}s (${LARGE_FL_MIB_S} MiB/s) integrity=$LARGE_FL_OK"
echo "wget     mean=${LARGE_WG_MEAN}s (${LARGE_WG_MIB_S} MiB/s) integrity=$LARGE_WG_OK"
echo

# --- multi ---
echo "=== multi (${MULTI_PARTS} x ${MULTI_PART_MIB} MiB = ${MULTI_TOTAL_MIB} MiB), ${MULTI_RUNS} timed runs after warmup ==="
echo "fetchling: --max-threads ${MAX_THREADS} --max-threads-per-host ${PER_HOST_THREADS} with -i urls.txt"
echo "wget:      sequential loop over the same URLs"
run_fl_multi "$OUT/fl-multi"
run_wg_multi "$OUT/wg-multi"
MULTI_FL_TIMES=()
MULTI_WG_TIMES=()
for _ in $(seq 1 "$MULTI_RUNS"); do
  start=$(date +%s.%N)
  run_fl_multi "$OUT/fl-multi"
  MULTI_FL_TIMES+=("$(elapsed "$start")")

  start=$(date +%s.%N)
  run_wg_multi "$OUT/wg-multi"
  MULTI_WG_TIMES+=("$(elapsed "$start")")
done
MULTI_FL_MEAN=$(printf '%s\n' "${MULTI_FL_TIMES[@]}" | mean)
MULTI_WG_MEAN=$(printf '%s\n' "${MULTI_WG_TIMES[@]}" | mean)
MULTI_FL_OK=ok
MULTI_WG_OK=ok
for i in $(seq -w 1 "$MULTI_PARTS"); do
  cmp -s "$OUT/fl-multi/part${i}.bin" "$DOCROOT/part${i}.bin" || MULTI_FL_OK=FAIL
  cmp -s "$OUT/wg-multi/part${i}.bin" "$DOCROOT/part${i}.bin" || MULTI_WG_OK=FAIL
done
MULTI_FL_MIB_S=$(awk -v t="$MULTI_FL_MEAN" -v m="$MULTI_TOTAL_MIB" 'BEGIN{printf "%.2f", m/t}')
MULTI_WG_MIB_S=$(awk -v t="$MULTI_WG_MEAN" -v m="$MULTI_TOTAL_MIB" 'BEGIN{printf "%.2f", m/t}')
echo "fetchling mean=${MULTI_FL_MEAN}s (${MULTI_FL_MIB_S} MiB/s) integrity=$MULTI_FL_OK"
echo "wget     mean=${MULTI_WG_MEAN}s (${MULTI_WG_MIB_S} MiB/s) integrity=$MULTI_WG_OK (sequential)"
echo

echo "=== summary ==="
awk -v fl="$SMALL_FL_MEAN" -v wg="$SMALL_WG_MEAN" 'BEGIN{
  printf "small:  fetchling %ss  wget %ss  ratio(fl/wg)=%.3f\n", fl, wg, fl/wg
}'
awk -v fl="$LARGE_FL_MEAN" -v wg="$LARGE_WG_MEAN" -v flm="$LARGE_FL_MIB_S" -v wgm="$LARGE_WG_MIB_S" 'BEGIN{
  printf "large:  fetchling %ss (%.2f MiB/s)  wget %ss (%.2f MiB/s)  ratio(fl/wg)=%.3f\n", fl, flm, wg, wgm, fl/wg
}'
awk -v fl="$MULTI_FL_MEAN" -v wg="$MULTI_WG_MEAN" -v flm="$MULTI_FL_MIB_S" -v wgm="$MULTI_WG_MIB_S" -v thr="$MAX_THREADS" -v per="$PER_HOST_THREADS" 'BEGIN{
  printf "multi:  fetchling %ss (%.2f MiB/s, --max-threads %s --max-threads-per-host %s)  wget %ss (%.2f MiB/s, sequential)  ratio(fl/wg)=%.3f\n", fl, flm, thr, per, wg, wgm, fl/wg
}'
echo "integrity: small fl=$SMALL_FL_OK wg=$SMALL_WG_OK | large fl=$LARGE_FL_OK wg=$LARGE_WG_OK | multi fl=$MULTI_FL_OK wg=$MULTI_WG_OK"
echo "note: localhost only; measures client stack + concurrency, not WAN RTT."

fail=0
[[ "$SMALL_FL_OK" == ok && "$SMALL_WG_OK" == ok ]] || fail=1
[[ "$LARGE_FL_OK" == ok && "$LARGE_WG_OK" == ok ]] || fail=1
[[ "$MULTI_FL_OK" == ok && "$MULTI_WG_OK" == ok ]] || fail=1
if [[ "$fail" -ne 0 ]]; then
  echo "wget-bench: integrity check failed" >&2
  exit 1
fi
echo "wget-bench: complete"
