#!/usr/bin/env bash
# Runs the Kani proofs in 08-verified-handle-map one harness at a time and
# prints a table of result, time and peak memory.
#
#   tools/run_kani.sh                       # all harnesses
#   tools/run_kani.sh add_is_correct        # harnesses whose name contains this
#   KANI_FEATURES=naive-generations tools/run_kani.sh
#
# Model checking can use gigabytes. A watchdog kills a proof when available
# memory drops below MIN_AVAIL_MB (default 800) so the machine stays usable.
set -u
cd "$(dirname "$0")/.."
source tools/watchdog.sh
filter="${1:-}"
features=()
[ -n "${KANI_FEATURES:-}" ] && features=(--features "$KANI_FEATURES")

# Every `fn` directly after a `#[kani::proof]` attribute (attributes may follow).
harnesses=$(grep -A4 'kani::proof' 08-verified-handle-map/src/proofs.rs \
  | grep -oE 'fn [a-z_0-9]+' | sed 's/^fn //' | sort -u)
modules=$(grep -oE '^harnesses!\([a-z0-9_]+' 08-verified-handle-map/src/proofs.rs | sed 's/harnesses!(//')

printf '%-60s %-9s %8s %10s\n' harness result seconds peak_MB
for module in $modules; do
  for h in $harnesses; do
    name="proofs::$module::$h"
    [[ -n "$filter" && "$name" != *"$filter"* ]] && continue
    log=$(mktemp)
    run_watched "$log" cargo kani -p verified_handle_map "${features[@]}" --exact --harness "$name"
    if [ -n "$killed" ]; then result=OOM-KILLED
    elif grep -q 'VERIFICATION:- SUCCESSFUL' "$log"; then result=OK
    elif grep -q 'VERIFICATION:- FAILED' "$log"; then result=FAILED
    else result=ERROR
    fi
    printf '%-60s %-9s %8s %10s\n' "$name" "$result" "$secs" "$((kb / 1024))"
    if [ "$result" = FAILED ] || [ "$result" = ERROR ]; then
      grep -E 'Failed Checks|^error' "$log" | head -5 | sed 's/^/    /'
    fi
    rm -f "$log"
  done
done
