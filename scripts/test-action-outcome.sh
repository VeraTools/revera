#!/usr/bin/env bash
# Offline tests for action-outcome.sh: exit-code classification, fail-on
# policy, stale/corrupt report handling.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

good="$tmp/good.json"
cat > "$good" <<'EOF'
{"status":"complete","findings":[
  {"validation_status":"accepted"},
  {"validation_status":"rejected"},
  {"validation_status":"accepted"}
]}
EOF
missing="$tmp/missing.json"
corrupt="$tmp/corrupt.json"
echo '{not json' > "$corrupt"

fails=0
# run CODE REPORT FAIL_ON -> sets $rc and $out (GITHUB_OUTPUT contents)
run() {
    local o="$tmp/out.txt"
    : > "$o"
    set +e
    GITHUB_OUTPUT="$o" "$HERE/action-outcome.sh" "$1" "$2" "$3" >/dev/null 2>&1
    rc=$?
    set -e
    out=$(cat "$o")
}
expect() { # <desc> <rc> <status> <findings> <report-path>
    local desc="$1" want_rc="$2" want_status="$3" want_findings="$4" want_report="$5"
    local got_status got_findings got_report
    got_status=$(sed -n 's/^status=//p' <<<"$out")
    got_findings=$(sed -n 's/^findings=//p' <<<"$out")
    got_report=$(sed -n 's/^report-path=//p' <<<"$out")
    if [ "$rc" != "$want_rc" ] || [ "$got_status" != "$want_status" ] \
        || [ "$got_findings" != "$want_findings" ] || [ "$got_report" != "$want_report" ]; then
        echo "FAIL: $desc -> rc=$rc status=$got_status findings=$got_findings report=$got_report" >&2
        fails=$((fails + 1))
    fi
}

# classification with fail-on: failed
run 0 "$good" failed;   expect "0/failed"   0 complete 2 "$good"
run 2 "$good" failed;   expect "2/failed"   0 partial  2 "$good"
run 1 "$good" failed;   expect "1/failed"   1 failed   0 ""
run 101 "$good" failed; expect "101/failed" 1 failed   0 ""
run 127 "$good" failed; expect "127/failed" 1 failed   0 ""
run 137 "$good" failed; expect "137/failed" 1 failed   0 ""

# fail-on: partial fails anything but complete
run 0 "$good" partial;  expect "0/partial"  0 complete 2 "$good"
run 2 "$good" partial;  expect "2/partial"  1 partial  2 "$good"
run 137 "$good" partial; expect "137/partial" 1 failed 0 ""

# fail-on: never always passes but reports truthfully
run 1 "$good" never;    expect "1/never"    0 failed   0 ""
run 2 "$good" never;    expect "2/never"    0 partial  2 "$good"

# invalid policy
run 0 "$good" sometimes; [ "$rc" = 1 ] || { echo "FAIL: invalid fail-on accepted" >&2; fails=$((fails + 1)); }

# exit 0 without a report is not a complete review
run 0 "$missing" failed; expect "0/no-report" 1 failed 0 ""
run 2 "$missing" never;  expect "2/no-report" 0 failed 0 ""

# corrupt report: status stands, findings unknown -> 0, path still exposed
run 0 "$corrupt" failed; expect "0/corrupt" 0 complete 0 "$corrupt"

if [ "$fails" -ne 0 ]; then
    echo "action-outcome.sh: $fails failure(s)" >&2
    exit 1
fi
echo "action-outcome.sh OK"
