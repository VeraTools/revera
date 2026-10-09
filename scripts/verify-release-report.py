#!/usr/bin/env python3
"""Verify two released Revera reports and their GitHub publication evidence."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


SUMMARY_MARKER = "<!-- revera-summary -->"
STATE_MARKER = re.compile(r"<!--\s*revera-state:([A-Za-z0-9+/=]+)\s*-->")
FINDING_MARKER = re.compile(r"<!--\s*revera-id:([0-9a-f]{12})\s*-->")
TAG_PATTERN = re.compile(r"^v(\d+\.\d+\.\d+)$")
MAX_PAGES = 50
REQUEST_TIMEOUT = 10


class ProofFailure(Exception):
    """A verification condition failed."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ProofFailure(message)


def load_report(path: str, name: str) -> dict[str, Any]:
    try:
        report = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError):
        raise ProofFailure(f"{name} could not be read as JSON") from None
    require(isinstance(report, dict), f"{name} is not a JSON object")
    return report


def positive_integer(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value > 0


def finding_id(finding: dict[str, Any]) -> str:
    file_name = finding.get("file")
    defect_key = finding.get("defect_key")
    require(
        isinstance(file_name, str) and isinstance(defect_key, str),
        "designated finding is missing file or defect_key",
    )
    return hashlib.sha256((file_name + "\0" + defect_key).encode("utf-8")).hexdigest()[:12]


def finding_text(finding: dict[str, Any]) -> str:
    return " ".join(
        str(finding.get(field, "")) for field in ("title", "claim", "trigger", "impact")
    ).casefold()


def match_designated_finding(
    report: dict[str, Any], expected_file: str, term_groups: list[list[str]]
) -> tuple[dict[str, Any], str]:
    findings = report.get("findings")
    require(isinstance(findings, list), "run 1 has no findings list")

    matches: list[dict[str, Any]] = []
    for finding in findings:
        if not isinstance(finding, dict):
            continue
        if finding.get("file") != expected_file:
            continue
        if finding.get("validation_status") != "accepted":
            continue
        haystack = finding_text(finding)
        if all(any(term.casefold() in haystack for term in group) for group in term_groups):
            matches.append(finding)

    require(
        len(matches) == 1,
        "run 1 must contain exactly one accepted finding at --expect-file matching every --expect-term group",
    )
    target = matches[0]
    target_id = finding_id(target)
    plan = report.get("plan")
    inline = plan.get("inline") if isinstance(plan, dict) else None
    require(isinstance(inline, list), "run 1 has no plan.inline list")
    has_target_inline = any(
        isinstance(comment, dict)
        and isinstance(comment.get("body"), str)
        and f"<!-- revera-id:{target_id} -->" in comment["body"]
        for comment in inline
    )
    require(
        has_target_inline,
        "designated accepted finding's revera-id is missing from run 1 plan.inline",
    )
    return target, target_id


def vera_search_stat(stats: dict[str, Any]) -> dict[str, Any] | None:
    tools = stats.get("tools")
    if not isinstance(tools, list):
        return None
    for tool in tools:
        if isinstance(tool, dict) and tool.get("name") == "vera_search":
            return tool
    return None


def require_reranked_search(stats1: dict[str, Any], probe_path: str | None) -> None:
    """Run 1 must report an active reranker, and reranking must be shown by an
    error-free vera_search call in run 1 or, when it made none, by a
    `vera search --json --rerank-status` probe against the same Vera home."""
    retrieval = stats1.get("retrieval")
    require(
        retrieval == "vera+rerank",
        "run 1 stats.retrieval is not exactly vera+rerank (reranker inactive, degraded, "
        "or fell back)",
    )
    search = vera_search_stat(stats1)
    if search is not None and positive_integer(search.get("calls")):
        errors = search.get("errors")
        require(
            isinstance(errors, int) and not isinstance(errors, bool) and errors == 0,
            "run 1 vera_search reported errors (errors must be 0)",
        )
        return
    require(
        probe_path is not None,
        "run 1 made no vera_search call and no --rerank-probe was given",
    )
    probe = load_report(probe_path, "rerank probe")
    require(
        probe.get("reranked") is True and probe.get("rerank_fallback_reason") is None,
        "rerank probe did not return reranked results",
    )


def validate_report_pair(
    args: argparse.Namespace,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any], str]:
    match = TAG_PATTERN.fullmatch(args.tag)
    require(match is not None, "--tag must be an exact stable vX.Y.Z tag")
    version = match.group(1)
    reported = re.fullmatch(r"revera\s+(\S+)", args.version_output.strip())
    require(
        reported is not None and reported.group(1) == version,
        "binary version output is not exactly `revera <tag version>`",
    )
    require(args.binary_sha256 is not None, "--binary-sha256 is required")
    require(
        re.fullmatch(r"[0-9a-fA-F]{64}", args.binary_sha256) is not None,
        "--binary-sha256 must be a 64-character hexadecimal SHA-256",
    )
    require(args.exit1 == 0, f"run 1 exited with status {args.exit1}")
    require(args.exit2 == 0, f"run 2 exited with status {args.exit2}")

    report1 = load_report(args.report1, "run 1 report")
    report2 = load_report(args.report2, "run 2 report")
    for number, report in ((1, report1), (2, report2)):
        require(
            report.get("status") == "complete",
            f"run {number} report status is not complete",
        )
        require(
            report.get("base") == args.base,
            f"run {number} report base does not match the expected base SHA",
        )
        require(
            report.get("head") == args.head,
            f"run {number} report head does not match the expected head SHA",
        )

    publication1 = report1.get("publication")
    require(isinstance(publication1, dict), "run 1 has no publication object")
    require(
        publication1.get("mode") == "comment",
        "run 1 publication.mode is not comment",
    )
    require(
        publication1.get("skipped_reason") is None,
        "run 1 publication has a skipped_reason",
    )

    stats1 = report1.get("stats")
    require(isinstance(stats1, dict), "run 1 has no stats object")
    require(
        stats1.get("validation") == "fresh",
        "run 1 validation is not fresh",
    )

    if args.require_rerank:
        require_reranked_search(stats1, args.rerank_probe)

    ledger = report1.get("ledger")
    routes = ledger.get("by_route") if isinstance(ledger, dict) else None
    require(isinstance(routes, list), "run 1 has no ledger.by_route list")
    for entry in routes:
        require(isinstance(entry, dict), "run 1 ledger contains a malformed route entry")
        route = entry.get("route")
        model = entry.get("model")
        role_value = entry.get("role")
        require(
            isinstance(route, str) and isinstance(model, str) and isinstance(role_value, str),
            "run 1 ledger contains a route entry without string route, model, and role",
        )
        require(
            "scripted" not in (route + " " + model).casefold(),
            "run 1 ledger contains a scripted route; live model traffic is required",
        )
    for role in ("investigator", "validator"):
        role_routes = [
            entry for entry in routes if isinstance(entry, dict) and entry.get("role") == role
        ]
        require(
            any(
                positive_integer(entry.get("requests"))
                and isinstance(entry.get("route"), str)
                and bool(entry["route"].strip())
                and "scripted" not in entry["route"].casefold()
                for entry in role_routes
            ),
            f"run 1 ledger has no live {role} route with requests > 0",
        )

    terms: list[list[str]] = []
    for group in args.expect_term:
        alternatives = [term.strip() for term in group.split("|")]
        require(
            all(alternatives),
            "--expect-term groups and alternatives must not be empty",
        )
        terms.append(alternatives)
    require(bool(terms), "at least one --expect-term group is required")
    finding, matched_id = match_designated_finding(report1, args.expect_file, terms)

    summary_id = publication1.get("summary_comment_id")
    require(
        positive_integer(summary_id),
        "run 1 publication.summary_comment_id must be a positive integer",
    )

    publication2 = report2.get("publication")
    require(isinstance(publication2, dict), "run 2 has no publication object")
    summary_id2 = publication2.get("summary_comment_id")
    require(
        positive_integer(summary_id2) and summary_id2 == summary_id,
        "run 2 summary_comment_id differs from run 1",
    )
    stats2 = report2.get("stats")
    require(
        isinstance(stats2, dict) and stats2.get("reused") is True, "run 2 stats.reused is not true"
    )
    require(
        stats2.get("validation") == "reused",
        "run 2 stats.validation is not reused",
    )
    require(
        publication2.get("review_id") is None,
        "run 2 publication.review_id is present for an identical-head reuse",
    )

    return report1, report2, finding, matched_id


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Do not forward the bearer token to an HTTP redirect target."""

    def redirect_request(self, _req, _fp, _code, _message, _headers, _newurl):
        return None


class GitHubApi:
    def __init__(self, base_url: str, token: str) -> None:
        parsed = urllib.parse.urlsplit(base_url)
        loopback = (parsed.hostname or "") in ("localhost", "127.0.0.1", "::1")
        require(
            bool(parsed.netloc)
            and (parsed.scheme == "https" or (parsed.scheme == "http" and loopback)),
            "--api-url must be an absolute https URL (http only for loopback)",
        )
        require(
            parsed.username is None
            and parsed.password is None
            and not parsed.query
            and not parsed.fragment,
            "--api-url must not contain credentials, a query, or a fragment",
        )
        self.base_url = base_url.rstrip("/")
        self.origin = (parsed.scheme.lower(), parsed.netloc.lower())
        self.token = token
        self.opener = urllib.request.build_opener(_NoRedirectHandler())

    def _get(self, url: str, label: str) -> tuple[Any, str | None]:
        request = urllib.request.Request(
            url,
            headers={
                "Accept": "application/vnd.github+json",
                "Authorization": f"Bearer {self.token}",
                "User-Agent": "revera-release-proof",
                "X-GitHub-Api-Version": "2022-11-28",
            },
        )
        try:
            with self.opener.open(request, timeout=REQUEST_TIMEOUT) as response:
                body = response.read()
                link_header = response.headers.get("Link")
        except urllib.error.HTTPError as error:
            raise ProofFailure(f"GitHub {label} request failed (HTTP {error.code})") from None
        except (urllib.error.URLError, TimeoutError, OSError):
            raise ProofFailure(f"GitHub {label} request failed (network error)") from None

        try:
            return json.loads(body), link_header
        except (UnicodeError, json.JSONDecodeError):
            raise ProofFailure(f"GitHub {label} response was not JSON") from None

    def get_json(self, path: str, label: str) -> Any:
        result, _ = self._get(self.base_url + path, label)
        return result

    @staticmethod
    def _next_link(link_header: str | None) -> str | None:
        if not link_header:
            return None
        for link in link_header.split(","):
            match = re.search(r"<([^>]+)>\s*;\s*rel\s*=\s*\"?([^\";]+)", link)
            if match and match.group(2).strip() == "next":
                return match.group(1)
        return None

    def list_json(self, path: str, label: str) -> list[Any]:
        url = self.base_url + path
        separator = "&" if "?" in url else "?"
        url = f"{url}{separator}per_page=100&page=1"
        results: list[Any] = []
        seen: set[str] = set()

        for page in range(1, MAX_PAGES + 1):
            require(url not in seen, f"GitHub {label} pagination repeated a page")
            seen.add(url)
            value, link_header = self._get(url, label)
            require(isinstance(value, list), f"GitHub {label} response was not a list")
            results.extend(value)
            next_url = self._next_link(link_header)
            if next_url:
                absolute = urllib.parse.urljoin(url, next_url)
                parsed = urllib.parse.urlsplit(absolute)
                require(
                    (parsed.scheme.lower(), parsed.netloc.lower()) == self.origin,
                    f"GitHub {label} pagination link changed API origin",
                )
                url = absolute
            elif len(value) >= 100:
                parsed = urllib.parse.urlsplit(url)
                query = urllib.parse.parse_qs(parsed.query)
                current_page = query.get("page", [str(page)])[0]
                try:
                    next_page = int(current_page) + 1
                except ValueError:
                    next_page = page + 1
                query["page"] = [str(next_page)]
                new_query = urllib.parse.urlencode(query, doseq=True)
                url = urllib.parse.urlunsplit(
                    (parsed.scheme, parsed.netloc, parsed.path, new_query, "")
                )
            else:
                return results
        raise ProofFailure(f"GitHub {label} exceeded {MAX_PAGES} pages")


def verify_remote(
    api: GitHubApi,
    repo: str,
    pr: int,
    expected_head: str,
    author: str,
    summary_id: int,
    run1: dict[str, Any],
    target_id: str,
) -> None:
    summary = api.get_json(
        f"/repos/{repo}/issues/comments/{summary_id}",
        "managed summary comment",
    )
    require(isinstance(summary, dict), "managed summary comment response was not an object")
    require(
        summary.get("id") == summary_id,
        "managed summary comment response id does not match run 1",
    )
    require(
        isinstance(summary.get("user"), dict) and summary["user"].get("login") == author,
        "managed summary comment author does not match --author",
    )
    body = summary.get("body")
    require(isinstance(body, str), "managed summary comment has no body")
    require(
        body.startswith(SUMMARY_MARKER),
        "managed summary comment is missing the managed marker at the start of its body",
    )

    # publish.rs embeds ReviewState in a revera-state base64 comment; its
    # reviewed_head is the durable head evidence because the visible summary
    # markdown itself does not identify the reviewed commit.
    state_match = STATE_MARKER.search(body)
    require(
        state_match is not None,
        "managed summary comment is missing its revera-state marker",
    )
    try:
        state_json = base64.b64decode(state_match.group(1), validate=True)
        state = json.loads(state_json)
    except (ValueError, UnicodeError, json.JSONDecodeError):
        raise ProofFailure("managed summary comment has an invalid revera-state marker") from None
    require(
        isinstance(state, dict) and state.get("reviewed_head") == expected_head,
        "managed summary state does not reference the expected head",
    )

    issue_comments = api.list_json(
        f"/repos/{repo}/issues/{pr}/comments",
        "issue comments",
    )
    managed = [
        comment
        for comment in issue_comments
        if isinstance(comment, dict)
        and isinstance(comment.get("user"), dict)
        and comment["user"].get("login") == author
        and isinstance(comment.get("body"), str)
        and comment["body"].startswith(SUMMARY_MARKER)
    ]
    require(
        len(managed) == 1,
        f"GitHub PR has {len(managed)} issue comments containing the managed marker; expected exactly one",
    )
    require(
        managed[0].get("id") == summary_id,
        "the PR's managed summary comment is not the one named by run 1",
    )

    inline = run1.get("plan", {}).get("inline", [])
    ids: set[str] = set()
    for comment in inline:
        if isinstance(comment, dict) and isinstance(comment.get("body"), str):
            ids.update(FINDING_MARKER.findall(comment["body"]))
    review_comments = api.list_json(
        f"/repos/{repo}/pulls/{pr}/comments",
        "pull request review comments",
    )
    duplicates = [
        revera_id
        for revera_id in sorted(ids)
        if sum(
            1
            for comment in review_comments
            if isinstance(comment, dict)
            and isinstance(comment.get("body"), str)
            and isinstance(comment.get("user"), dict)
            and comment["user"].get("login") == author
            and revera_id in FINDING_MARKER.findall(comment["body"])
        )
        > 1
    ]
    require(
        not duplicates,
        "GitHub has duplicate inline review comments for run 1 revera-id(s): "
        + ", ".join(duplicates),
    )
    target_comments = [
        comment
        for comment in review_comments
        if isinstance(comment, dict)
        and isinstance(comment.get("body"), str)
        and target_id in FINDING_MARKER.findall(comment["body"])
        and isinstance(comment.get("user"), dict)
        and comment["user"].get("login") == author
        and comment.get("commit_id") == expected_head
    ]
    require(
        len(target_comments) == 1,
        f"GitHub has {len(target_comments)} inline comments by --author for the designated "
        "finding; expected exactly one",
    )

    reviews = api.list_json(
        f"/repos/{repo}/pulls/{pr}/reviews",
        "pull request reviews",
    )
    matching_reviews = [
        review
        for review in reviews
        if isinstance(review, dict)
        and isinstance(review.get("user"), dict)
        and review["user"].get("login") == author
        and review.get("commit_id") == expected_head
    ]
    require(
        len(matching_reviews) == 1,
        f"GitHub has {len(matching_reviews)} reviews by --author for the expected head; expected exactly one",
    )
    review_id = run1.get("publication", {}).get("review_id")
    require(
        not positive_integer(review_id) or matching_reviews[0].get("id") == review_id,
        "GitHub review by --author for the expected head does not match run 1 review_id",
    )


def markdown_value(value: str) -> str:
    value = re.sub(
        r"(?i)\b(?:gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}|sk-[A-Za-z0-9_-]{16,})\b",
        "[redacted]",
        value,
    )
    value = re.sub(
        r"(?i)\b(?:bearer\s+\S+|(?:api[-_ ]?key|token|secret|password)\s*[:=]\s*[^\s,;]+)",
        "[redacted]",
        value,
    )
    value = re.sub(r"https?://[^\s)\]>]+", _safe_url, value)
    value = " ".join(value.split())
    return value.replace("|", "\\|").replace("`", "'")


def _safe_url(match: re.Match[str]) -> str:
    try:
        parsed = urllib.parse.urlsplit(match.group(0))
        host = parsed.hostname or ""
        if parsed.port:
            host += f":{parsed.port}"
        return f"{parsed.scheme}://{host}"
    except ValueError:
        return "[redacted-url]"


def route_evidence(report: dict[str, Any]) -> list[str]:
    ledger = report.get("ledger", {})
    routes = ledger.get("by_route", []) if isinstance(ledger, dict) else []
    evidence: list[str] = []
    for entry in routes:
        if not isinstance(entry, dict):
            continue
        requests = entry.get("requests", 0)
        if not isinstance(requests, int) or isinstance(requests, bool) or requests < 0:
            requests = 0
        route = str(entry.get("route", "unknown"))
        protocol = route.split(":", 1)[0]
        evidence.append(
            "- "
            + markdown_value(str(entry.get("role", "unknown")))
            + ": protocol `"
            + markdown_value(protocol)
            + "`, model `"
            + markdown_value(str(entry.get("model", "unknown")))
            + "`, requests "
            + str(requests)
        )
    return evidence or ["- (no routes recorded)"]


def write_summary(
    path: str,
    args: argparse.Namespace,
    report1: dict[str, Any],
    finding: dict[str, Any],
    matched_id: str,
) -> None:
    publication = report1.get("publication", {})
    review_id = publication.get("review_id") if isinstance(publication, dict) else None
    route_lines = "\n".join(route_evidence(report1))
    stats = report1.get("stats")
    stats = stats if isinstance(stats, dict) else {}
    retrieval = markdown_value(str(stats.get("retrieval", "unknown")))
    search = vera_search_stat(stats)
    calls = search.get("calls") if search else 0
    search_calls = calls if isinstance(calls, int) and not isinstance(calls, bool) else 0
    probe = ""
    if args.require_rerank and args.rerank_probe and search_calls == 0:
        probe = ", reranking shown by the rerank probe"
    title = markdown_value(str(finding.get("title", "")))
    version_output = markdown_value(args.version_output)
    content = (
        "## Release proof evidence\n\n"
        f"- Tag: `{markdown_value(args.tag)}`\n"
        f"- Version: `{version_output}`\n"
        f"- Binary SHA-256: `{args.binary_sha256.lower()}`\n"
        f"- Base: `{markdown_value(args.base)}`\n"
        f"- Head: `{markdown_value(args.head)}`\n"
        "- Routes/models used:\n"
        f"{route_lines}\n"
        f"- Retrieval: `{retrieval}`, vera_search calls {search_calls}{probe}\n"
        f"- Summary comment ID: `{publication['summary_comment_id']}`\n"
        f"- Review ID: `{review_id if positive_integer(review_id) else 'none'}`\n"
        f"- Matched finding ID: `{matched_id}` — {title}\n"
    )
    try:
        Path(path).write_text(content, encoding="utf-8")
    except OSError:
        raise ProofFailure("could not write --out-summary") from None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report1", required=True)
    parser.add_argument("--report2", required=True)
    parser.add_argument("--exit1", required=True, type=int)
    parser.add_argument("--exit2", required=True, type=int)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--pr", required=True, type=int)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--version-output", required=True)
    parser.add_argument("--binary-sha256", required=True)
    parser.add_argument("--expect-file", required=True)
    parser.add_argument("--expect-term", action="append", required=True)
    parser.add_argument("--author", required=True)
    parser.add_argument("--api-url", default="https://api.github.com")
    parser.add_argument("--out-summary", required=True)
    parser.add_argument(
        "--require-rerank",
        action="store_true",
        help="require run 1 to report vera+rerank, plus an error-free vera_search call "
        "or a passing --rerank-probe",
    )
    parser.add_argument(
        "--rerank-probe",
        help="`vera search --json --rerank-status` output from the run's Vera home",
    )
    args = parser.parse_args()
    require(
        re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo) is not None,
        "--repo must be owner/name",
    )
    require(args.pr > 0, "--pr must be a positive integer")
    require(
        re.fullmatch(r"[0-9a-fA-F]{40}", args.base) is not None
        and re.fullmatch(r"[0-9a-fA-F]{40}", args.head) is not None,
        "--base and --head must be full 40-character commit SHAs",
    )
    return args


def main() -> int:
    try:
        args = parse_args()
        report1, _report2, finding, matched_id = validate_report_pair(args)
        token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        require(bool(token), "missing GH_TOKEN or GITHUB_TOKEN for GitHub verification")
        publication = report1["publication"]
        api = GitHubApi(args.api_url, token)
        verify_remote(
            api,
            args.repo,
            args.pr,
            args.head,
            args.author,
            publication["summary_comment_id"],
            report1,
            matched_id,
        )
        write_summary(args.out_summary, args, report1, finding, matched_id)
    except ProofFailure as error:
        print(f"release proof: FAIL: {error}", file=sys.stderr)
        return 1
    except Exception as error:  # Fail closed without printing report or secret data.
        print(
            f"release proof: FAIL: unexpected verification error ({type(error).__name__})",
            file=sys.stderr,
        )
        return 1
    print("release proof: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
