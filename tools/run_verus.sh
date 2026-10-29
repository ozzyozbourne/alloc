#!/usr/bin/env bash
# Verifies 09-verus-handle-map with Verus and prints the result, time and
# peak memory. Extra arguments go to verus, for example `--compile`.
#
#   tools/run_verus.sh
#   tools/run_verus.sh --compile -o /tmp/verus-demo && /tmp/verus-demo
#
# VERUS is the verus binary (default: verus on PATH). VERUS_Z3_PATH must point
# at Z3 4.16.0 if the bundled z3 does not run on this machine.
# A watchdog (tools/watchdog.sh) kills verus and z3 when available memory
# drops below MIN_AVAIL_MB (default 800) so the machine stays usable.
set -u
cd "$(dirname "$0")/.."
source tools/watchdog.sh
verus="${VERUS:-verus}"
log=$(mktemp)
run_watched "$log" "$verus" 09-verus-handle-map/src/main.rs "$@"

# Verus built against a newer glibc warns about it on every run; hide that.
grep -v 'GLIBC_2.39' "$log"
if [ -n "$killed" ]; then echo "OOM-KILLED"; status=1; fi
echo "time: ${secs} s, peak memory: $((kb / 1024)) MB"
rm -f "$log"
exit "$status"
