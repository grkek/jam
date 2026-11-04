#!/usr/bin/env bash
# Boots the kernel under QEMU, waits for the stress plan to finish, then checks the log.
#
# usage: boot-test.sh LOG TIMEOUT_SECONDS COMMAND [ARGS...]
#
# Works on Linux and macOS (bash 3.2). QEMU `virt` powers itself off when the plan
# is done; the Raspberry Pi 4 has no power interface, so the script stops it.
set -uo pipefail

log="$1"
timeout_seconds="$2"
shift 2

"$@" >"$log" 2>&1 &
runner=$!

for ((elapsed = 0; elapsed < timeout_seconds; elapsed++)); do
  grep -q "Stress done:" "$log" && break
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

grep -q "Kernel bootstrap sequence complete." "$log" || fail "kernel did not finish bootstrapping"
grep -q "Stress done:" "$log" || fail "stress plan did not finish within ${timeout_seconds}s"
if grep -Eq "panicked|Fatal:" "$log"; then
  fail "kernel reported a panic or fatal error"
fi

core_line=$(grep "Stress core complete:" "$log" | tail -n1)
if [[ -z "$core_line" ]]; then
  fail "no 'Stress core complete' line in the log"
else
  field() { grep -oE "[0-9]+ $1" <<<"$core_line" | head -n1 | cut -d' ' -f1; }

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

if [[ $failures -ne 0 ]]; then
  echo "---- last 60 lines of $log ----"
  tail -n 60 "$log"
  exit 1
fi
echo "Boot test passed."