# Sourced by run_kani.sh and run_verus.sh.
#
#   run_watched LOG CMD...
#
# Runs CMD with stdout and stderr in LOG, in its own process group. If
# available memory drops below MIN_AVAIL_MB (default 800), the whole group
# (including solvers like cbmc or z3) is killed. Sets:
#   status  exit status of CMD
#   killed  1 if the watchdog killed it, else empty
#   secs    wall-clock seconds
#   kb      peak memory in KB
run_watched() {
  local log="$1"
  shift
  local min_avail="${MIN_AVAIL_MB:-800}"
  setsid /usr/bin/time -f '%e %M' -o "$log.time" "$@" >"$log" 2>&1 &
  local pid=$!
  killed=""
  while kill -0 "$pid" 2>/dev/null; do
    if [ "$(free -m | awk '/Mem:/{print $7}')" -lt "$min_avail" ]; then
      kill -- "-$pid" 2>/dev/null
      killed=1
      break
    fi
    sleep 1
  done
  wait "$pid" 2>/dev/null
  status=$?
  read -r secs kb < <(tail -1 "$log.time") 2>/dev/null || { secs=-; kb=0; }
  rm -f "$log.time"
}
