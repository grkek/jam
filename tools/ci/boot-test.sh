
#!/usr/bin/env bash
# Boots the kernel under QEMU, waits for a test suite to finish, then checks the log.
#
# usage: boot-test.sh LOG TIMEOUT_SECONDS [--residency] COMMAND [ARGS...]
#
# With no extra option, runs the original stress-plan checks. With --residency,
# checks every stage of the local actor-lifecycle regression suite instead.
# Works on Linux and macOS (bash 3.2). QEMU `virt` powers itself off when the plan
# is done; the Raspberry Pi 4 has no power interface, so the script stops it.
set -uo pipefail

log="$1"
timeout_seconds="$2"
shift 2

suite="stress"
completion_marker="Stress done:"
if [[ "${1:-}" == "--residency" ]]; then
  suite="residency"
  completion_marker="Local actor lifecycle suite passed:"
  shift
fi

"$@" >"$log" 2>&1 &
runner=$!

for ((elapsed = 0; elapsed < timeout_seconds; elapsed++)); do
  grep -Fq "$completion_marker" "$log" && break
  kill -0 "$runner" 2>/dev/null || break
  sleep 1
done

pkill -f qemu-system-aarch64 2>/dev/null || true
kill "$runner" 2>/dev/null || true
wait "$runner" 2>/dev/null || true

failures=0
fail() {
  echo "::error::$*"
  failures=$((failures + 1))
}

require_log_marker() {
  grep -Fq "$1" "$log" || fail "$suite: missing completion marker '$1'"
}

require_log_marker "Kernel bootstrap sequence complete."
require_log_marker "$completion_marker"

if grep -Eq "panicked|Fatal:" "$log"; then
  fail "kernel reported a panic or fatal error"
fi

if [[ "$suite" == "residency" ]]; then
  # Validate the complete chain, so an accidentally skipped phase cannot pass CI.
  require_log_marker "Residency passed: 1000 EL0 actors alive and waiting, zero outstanding bootstrap messages"
  require_log_marker "Residency reclamation passed: 1000 actors recycled, all frames and message charges restored"
  require_log_marker "Wake/terminate races passed: 192 rounds,"
  require_log_marker "remote dispatches verified, zero frame/message drift"
  require_log_marker "Sustained load passed: 3 waves, 816 actor lifetimes, 24576 validated messages, zero frame/message drift"
  require_log_marker "Failure recovery passed: 32 queued-message victims, 32 EL0 stale-address probes, 16384 healthy messages, zero frame/message drift"
  require_log_marker "Local actor lifecycle suite passed: 32 reincarnations, 16384 EL0 stale sends rejected, 4096 fresh messages and 8192 background messages validated, zero frame/message drift, all actor slots clear"
else
  # Keep the original stress-plan checks, including deliberate fault accounting.
  core_line=$(grep "Stress core complete:" "$log" | tail -n1)

  if [[ -z "$core_line" ]]; then
    fail "no 'Stress core complete' line in the log"
  else
    field() {
      grep -oE "[0-9]+ $1" <<<"$core_line" | head -n1 | cut -d' ' -f1
    }

    spawn_failures=$(field "spawn failures")
    stranded=$(field "stranded")
    expected_faults=$(field "deliberate faults expected")
    stale=$(field "stale messages accepted")
    observed_faults=$(sed '/Stress core complete:/q' "$log" | grep -c " fault on core " || true)

    echo "spawn failures: $spawn_failures, stranded: $stranded, stale: $stale"
    echo "faults: expected $expected_faults, observed $observed_faults"

    [[ "$spawn_failures" == 0 ]] || fail "$spawn_failures spawn failures"
    [[ "$stranded" == 0 ]] || fail "$stranded actors never received a bootstrap message"
    [[ "$stale" == 0 ]] || fail "dead actors accepted $stale messages"
    [[ "$observed_faults" == "$expected_faults" ]] \
      || fail "expected $expected_faults actor faults but the log shows $observed_faults"
  fi
fi

if [[ $failures -ne 0 ]]; then
  echo "---- last 60 lines of $log ----"
  tail -n 60 "$log"
  exit 1
fi

echo "Boot test passed ($suite)."
