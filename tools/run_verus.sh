#!/usr/bin/env bash
# Verifies 09-verus-handle-map with Verus and prints the result, time and
# peak memory. Extra arguments go to verus, for example `--compile`.
#
#   tools/run_verus.sh
#   tools/run_verus.sh --compile -o /tmp/verus-demo && /tmp/verus-demo
#
# VERUS is the verus binary (default: verus on PATH). VERUS_Z3_PATH must point
# at Z3 4.16.0 if the bundled z3 does not run on this machine.
# A watchdog kills verus when available memory drops below MIN_AVAIL_MB
# (default 800) so the machine stays usable.
set -u
cd "$(dirname "$0")/.."
verus="${VERUS:-verus}"
min_avail="${MIN_AVAIL_MB:-800}"
log=$(mktemp)

/usr/bin/time -f '%e %M' -o "$log.time" \
  "$verus" 09-verus-handle-map/src/main.rs "$@" >"$log" 2>&1 &
pid=$!
killed=""
while kill -0 "$pid" 2>/dev/null; do
  avail=$(free -m | awk '/Mem:/{print $7}')
  if [ "$avail" -lt "$min_avail" ]; then
    pkill -P "$pid"; kill "$pid" 2>/dev/null; killed=1; break
  fi
  sleep 1
done
wait "$pid" 2>/dev/null
status=$?

# Verus built against a newer glibc warns about it on every run; hide that.
grep -v 'GLIBC_2.39' "$log"
read -r secs kb < <(tail -1 "$log.time") 2>/dev/null || { secs=-; kb=0; }
if [ -n "$killed" ]; then echo "OOM-KILLED"; status=1; fi
echo "time: ${secs} s, peak memory: $((kb / 1024)) MB"
rm -f "$log" "$log.time"
exit "$status"
