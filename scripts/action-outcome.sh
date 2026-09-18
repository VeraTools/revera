#!/usr/bin/env bash
# Classifies a `revera review` process exit into the Action's outcome and
# applies the fail-on policy. Used by action.yml; testable offline.
#
#   action-outcome.sh CODE REPORT_PATH FAIL_ON
#
# Writes status / report-path / findings to $GITHUB_OUTPUT (when set) and
# exits 0 or 1 according to FAIL_ON.
#   CODE 0 -> complete, 2 -> partial, anything else -> failed
#   (101 = Rust panic, 127 = binary missing, 137 = SIGKILL, ...)
# The report is only trusted for complete/partial runs; a failed process
# never gets findings counted from whatever file happens to be on disk.
set -euo pipefail

code=${1:?usage: action-outcome.sh CODE REPORT_PATH FAIL_ON}
report=${2:?usage: action-outcome.sh CODE REPORT_PATH FAIL_ON}
fail_on=${3:?usage: action-outcome.sh CODE REPORT_PATH FAIL_ON}

case "$code" in
  0) status=complete ;;
  2) status=partial ;;
  *) status=failed ;;
esac

report_out=""
findings=0
if [ "$status" != failed ] && [ -f "$report" ]; then
  report_out="$report"
  findings=$(python3 - "$report" <<'PY'
import json, sys
try:
    rep = json.load(open(sys.argv[1]))
    print(len([f for f in rep.get("findings", []) if f.get("validation_status") == "accepted"]))
except Exception as e:  # corrupt report -> no trustworthy count
    print(0)
    sys.stderr.write(f"revera: cannot read report: {e}\n")
PY
)
elif [ "$status" != failed ]; then
  echo "revera: process exited $code but no report at $report" >&2
  status=failed
fi

out=${GITHUB_OUTPUT:-/dev/null}
{
  echo "status=$status"
  echo "report-path=$report_out"
  echo "findings=$findings"
} >> "$out"

case "$status" in
  complete) echo "revera: complete, $findings finding(s)" ;;
  partial)  echo "revera: partial review (exit 2), $findings finding(s) so far" ;;
  failed)   echo "revera: review FAILED (exit $code)" >&2 ;;
esac

case "$fail_on" in
  failed)  [ "$status" = failed ] && exit 1; exit 0 ;;
  partial) [ "$status" != complete ] && exit 1; exit 0 ;;
  never)   exit 0 ;;
  *) echo "invalid fail-on: $fail_on (expected failed | partial | never)" >&2; exit 1 ;;
esac
