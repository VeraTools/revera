#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
cleanup() {
    python3 -c 'import shutil, sys; shutil.rmtree(sys.argv[1])' "$tmp"
}
trap cleanup EXIT

timeout 180s python3 - "$HERE/verify-release-report.py" "$tmp" <<'PY'
import base64
import hashlib
import http.server
import json
import os
import subprocess
import sys
import threading
import urllib.parse
from pathlib import Path

helper = Path(sys.argv[1])
tmp = Path(sys.argv[2])
BASE = "a" * 40
HEAD = "b" * 40
REPO = "owner/name"
PR = 12
AUTHOR = "github-actions[bot]"
SUMMARY_ID = 77
FINDING = {
    "defect_key": "null-check",
    "severity": "high",
    "file": "src/fixture.rs",
    "start_line": 21,
    "title": "Missing null guard causes panic",
    "claim": "A null input panics in the request path.",
    "trigger": "Trigger: submit a null input.",
    "impact": "The request panics.",
    "validation_status": "accepted",
}
FINDING_ID = hashlib.sha256(
    (FINDING["file"] + "\0" + FINDING["defect_key"]).encode()
).hexdigest()[:12]
TOKEN = "test-token-must-never-be-printed"
scenario = {"name": "good"}


def json_response(handler, value, link=None):
    body = json.dumps(value).encode()
    handler.send_response(200)
    handler.send_header("Content-Type", "application/json")
    if link:
        handler.send_header("Link", link)
    handler.send_header("Content-Length", str(len(body)))
    handler.end_headers()
    handler.wfile.write(body)


class ApiHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self.send_response(401)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        parsed = urllib.parse.urlsplit(self.path)
        query = urllib.parse.parse_qs(parsed.query)
        mode = scenario["name"]
        if parsed.path == f"/repos/{REPO}/issues/comments/{SUMMARY_ID}":
            reviewed_head = "d" * 40 if mode == "wrong-state-head" else HEAD
            state = base64.b64encode(
                json.dumps({"reviewed_head": reviewed_head, "reviewed_base": BASE}).encode()
            ).decode()
            author = "wrong-user" if mode == "wrong-comment-author" else AUTHOR
            body = f"<!-- revera-summary -->\n## Revera review\n<!-- revera-state:{state} -->"
            return json_response(
                self,
                {"id": SUMMARY_ID, "body": body, "user": {"login": author}},
            )
        if parsed.path == f"/repos/{REPO}/issues/{PR}/comments":
            page = query.get("page", ["1"])[0]
            if page == "1":
                items = [{"id": 1, "body": "unrelated", "user": {"login": "other"}}]
                link = (
                    f'<http://{self.server.server_address[0]}:'
                    f'{self.server.server_address[1]}{parsed.path}?per_page=100&page=2>; rel="next"'
                )
                return json_response(self, items, link)
            managed = {
                "id": SUMMARY_ID,
                "body": "<!-- revera-summary --> managed",
                "user": {"login": AUTHOR},
            }
            other_managed = {
                "id": SUMMARY_ID + 1,
                "body": "<!-- revera-summary --> managed duplicate",
                "user": {"login": "other"},
            }
            nonprefix_managed = {
                "id": SUMMARY_ID + 1,
                "body": "not managed <!-- revera-summary -->",
                "user": {"login": AUTHOR},
            }
            if mode == "duplicate-managed-summary":
                items = [managed, managed]
            elif mode == "duplicate-managed-summary-other":
                items = [managed, other_managed]
            elif mode == "nonprefix-managed-summary":
                items = [managed, nonprefix_managed]
            else:
                items = [managed]
            return json_response(self, items)
        if parsed.path == f"/repos/{REPO}/pulls/{PR}/comments":
            inline = {
                "id": 8,
                "body": f"Inline review <!-- revera-id:{FINDING_ID} -->",
                "user": {"login": AUTHOR},
                "commit_id": "d" * 40 if mode == "stale-inline-commit" else HEAD,
            }
            if mode == "missing-inline-review":
                return json_response(self, [])
            if mode == "duplicate-inline-review-other":
                other_inline = dict(inline, id=9, user={"login": "other"})
                return json_response(self, [inline, other_inline])
            return json_response(
                self,
                [inline, inline] if mode == "duplicate-inline-review" else [inline],
            )
        if parsed.path == f"/repos/{REPO}/pulls/{PR}/reviews":
            if mode == "missing-review":
                return json_response(self, [])
            review_id = 91 if mode == "wrong-review-id" else 90
            return json_response(
                self,
                [{"id": review_id, "commit_id": HEAD, "user": {"login": AUTHOR}}],
            )
        self.send_error(404)

    def log_message(self, _format, *_args):
        return


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), ApiHandler)
server.daemon_threads = True
thread = threading.Thread(target=server.serve_forever, daemon=True)
thread.start()
api_url = f"http://127.0.0.1:{server.server_address[1]}"
report1_path = tmp / "report1.json"
report2_path = tmp / "report2.json"
summary_path = tmp / "proof.md"


def report(number):
    finding = dict(FINDING)
    inline = {
        "file": finding["file"],
        "line": finding["start_line"],
        "body": f"Finding body <!-- revera-id:{FINDING_ID} -->",
    }
    return {
        "status": "complete",
        "base": BASE,
        "head": HEAD,
        "findings": [finding],
        "plan": {"inline": [inline]},
        "ledger": {
            "by_route": [
                {
                    "role": "investigator",
                    "route": "openai-chat:https://provider.example/v1",
                    "model": "investigator-model",
                    "requests": 2,
                },
                {
                    "role": "validator",
                    "route": "openai-chat:https://validator.example/v1",
                    "model": "validator-model",
                    "requests": 1,
                },
            ]
        },
        "stats": {
            "validation": "reused" if number == 2 else "fresh",
            "reused": number == 2,
        },
        "publication": {
            "mode": "comment",
            "summary_comment_id": SUMMARY_ID,
            **({"review_id": 90} if number == 1 else {}),
        },
    }


def save_reports(first=None, second=None):
    one = report(1) if first is None else first
    two = report(2) if second is None else second
    report1_path.write_text(json.dumps(one), encoding="utf-8")
    report2_path.write_text(json.dumps(two), encoding="utf-8")


def invoke(
    name,
    *,
    first=None,
    second=None,
    exit1=0,
    tag="v0.4.0",
    version_output="revera 0.4.0",
    api=None,
):
    scenario["name"] = name
    save_reports(first, second)
    env = dict(os.environ)
    env.update({"GH_TOKEN": TOKEN, "MOCK_SCENARIO": name})
    cmd = [
        sys.executable,
        str(helper),
        "--report1",
        str(report1_path),
        "--report2",
        str(report2_path),
        "--exit1",
        str(exit1),
        "--exit2",
        "0",
        "--repo",
        REPO,
        "--pr",
        str(PR),
        "--base",
        BASE,
        "--head",
        HEAD,
        "--tag",
        tag,
        "--version-output",
        version_output,
        "--binary-sha256",
        "c" * 64,
        "--expect-file",
        "src/fixture.rs",
        "--expect-term",
        "null|nil",
        "--expect-term",
        "panic",
        "--author",
        AUTHOR,
        "--api-url",
        api or api_url,
        "--out-summary",
        str(summary_path),
    ]
    return subprocess.run(
        cmd,
        text=True,
        capture_output=True,
        env=env,
        timeout=15,
        check=False,
    )


def expect_pass(name):
    result = invoke(name)
    if result.returncode != 0 or "release proof: PASS" not in result.stdout:
        raise AssertionError(
            f"{name}: expected PASS, got rc={result.returncode}; "
            f"stdout={result.stdout!r}; stderr={result.stderr!r}"
        )


def expect_fail(name, message, **kwargs):
    result = invoke(name, **kwargs)
    output = result.stdout + result.stderr
    if result.returncode == 0 or message not in output:
        raise AssertionError(
            f"{name}: expected nonzero and {message!r}, got rc={result.returncode}; "
            f"stdout={result.stdout!r}; stderr={result.stderr!r}"
        )
    if TOKEN in output:
        raise AssertionError(f"{name}: helper printed the GitHub token")


try:
    expect_pass("good")
    if not summary_path.exists():
        raise AssertionError("good case did not write the proof summary")
    summary_text = summary_path.read_text(encoding="utf-8")
    for expected in ("v0.4.0", "c" * 64, BASE, HEAD, FINDING_ID, "investigator-model"):
        if expected not in summary_text:
            raise AssertionError(f"proof summary omitted expected evidence {expected!r}")

    unrelated = report(1)
    unrelated["findings"][0]["title"] = "Unrelated accepted finding"
    unrelated["findings"][0]["claim"] = "No matching issue here."
    unrelated["findings"][0]["trigger"] = "A neutral condition occurs."
    unrelated["findings"][0]["impact"] = "A neutral result follows."
    expect_fail(
        "unrelated-accepted",
        "exactly one accepted finding at --expect-file matching every --expect-term group",
        first=unrelated,
    )

    no_summary = report(1)
    no_summary["publication"]["summary_comment_id"] = None
    expect_fail(
        "null-summary-id",
        "summary_comment_id must be a positive integer",
        first=no_summary,
    )
    absent_summary = report(1)
    del absent_summary["publication"]["summary_comment_id"]
    expect_fail(
        "absent-summary-id",
        "summary_comment_id must be a positive integer",
        first=absent_summary,
    )

    wrong_head = report(1)
    wrong_head["head"] = "d" * 40
    expect_fail(
        "wrong-report-head",
        "run 1 report head does not match the expected head SHA",
        first=wrong_head,
    )
    expect_fail("wrong-version", "binary version output is not exactly", tag="v0.5.0")
    expect_fail(
        "prerelease-version",
        "binary version output is not exactly",
        version_output="revera 0.4.0-rc.1",
    )
    expect_fail(
        "plain-http-api",
        "--api-url must be an absolute https URL",
        api="http://api.example.invalid",
    )
    expect_fail(
        "missing-inline-review",
        "inline comments by --author for the designated finding; expected exactly one",
    )
    expect_fail(
        "stale-inline-commit",
        "inline comments by --author for the designated finding; expected exactly one",
    )
    expect_fail("missing-review", "reviews by --author for the expected head; expected exactly one")
    expect_fail("wrong-review-id", "does not match run 1 review_id")
    expect_fail(
        "nonzero-exit",
        "run 1 exited with status 2",
        exit1=2,
    )
    partial = report(1)
    partial["status"] = "partial"
    expect_fail("partial-status", "run 1 report status is not complete", first=partial)

    scripted_investigator = report(1)
    scripted_investigator["ledger"]["by_route"][0]["route"] = "scripted:investigator"
    expect_fail(
        "scripted-investigator",
        "scripted route; live model traffic is required",
        first=scripted_investigator,
    )
    scripted_validator = report(1)
    scripted_validator["ledger"]["by_route"][1]["route"] = "scripted:validator"
    expect_fail(
        "scripted-validator",
        "scripted route; live model traffic is required",
        first=scripted_validator,
    )

    expect_fail(
        "duplicate-managed-summary",
        "issue comments containing the managed marker; expected exactly one",
    )
    expect_fail(
        "duplicate-inline-review",
        "duplicate inline review comments for run 1 revera-id",
    )
    expect_pass("duplicate-inline-review-other")
    expect_pass("duplicate-managed-summary-other")
    expect_pass("nonprefix-managed-summary")

    different_summary = report(2)
    different_summary["publication"]["summary_comment_id"] = SUMMARY_ID + 1
    expect_fail(
        "different-summary-id",
        "run 2 summary_comment_id differs from run 1",
        second=different_summary,
    )

    not_reused = report(2)
    not_reused["stats"]["reused"] = False
    expect_fail(
        "run-two-not-reused",
        "run 2 stats.reused is not true",
        second=not_reused,
    )

    wrong_reuse_validation = report(2)
    wrong_reuse_validation["stats"]["validation"] = "fresh"
    expect_fail(
        "run-two-reused-without-validation",
        "run 2 stats.validation is not reused",
        second=wrong_reuse_validation,
    )

    new_review = report(2)
    new_review["publication"]["review_id"] = 91
    expect_fail(
        "run-two-new-review",
        "run 2 publication.review_id is present",
        second=new_review,
    )

    expect_fail(
        "wrong-comment-author",
        "managed summary comment author does not match --author",
    )

    expect_fail(
        "wrong-state-head",
        "state does not reference the expected head",
    )

    print("test-verify-release-report.sh OK")
finally:
    server.shutdown()
    server.server_close()
    thread.join(timeout=2)
    if thread.is_alive():
        raise AssertionError("mock GitHub API server did not stop")
PY
