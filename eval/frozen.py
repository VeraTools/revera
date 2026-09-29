#!/usr/bin/env python3
"""Frozen-candidate comparison: validate one fixed candidate set under
several arms so validator-side differences are not confounded by
investigator variance.

  eval/frozen.py --candidates REPORT.json --repo REPO --base REV --head REV \
      --out DIR [--truth TRUTH.json] [--allow-config-diff] ARM.yaml [ARM.yaml ...]

REPORT.json must come from a `review.validate: false` run (its findings are
unvalidated candidates). Each arm config is used as-is except that its
investigator route is replaced by a scripted route replaying the frozen
candidates, its effective validator (the explicit validator, else the
original investigator route) is pinned as an explicit validator first,
`review.validate` is forced on, and `review.min_severity` is recorded for
scoring but lowered to `low` for the run. The script fails unless every
arm validated the identical candidate set (same digest) with one fresh
validator session per candidate, and unless arms differ only in their
validator route (`--allow-config-diff` records differences instead). It
prints one JSON line per arm and writes summary.json with the provenance.

TRUTH.json maps defect_key to {"label": "true"|"false"} plus optional
"high_impact": true and "fix_safe": false (the candidate's own
suggested_fix is wrong). It stays outside the reviewed repository and is
never shown to a model.
"""

import argparse
import copy
import hashlib
import json
import os
import shutil
import subprocess
import sys

import yaml

CANDIDATE_FIELDS = (
    "defect_key",
    "severity",
    "file",
    "start_line",
    "end_line",
    "title",
    "claim",
    "trigger",
    "impact",
    "introduced_by_change",
    "supporting_evidence",
    "suggested_fix",
)
SEVERITY_RANK = {"low": 0, "medium": 1, "high": 2}


def candidates(report):
    if report.get("stats", {}).get("validation") != "disabled":
        sys.exit("candidates report must come from a review.validate: false run")
    out = []
    for f in report["findings"]:
        out.append({k: f[k] for k in CANDIDATE_FIELDS if f.get(k) is not None})
    return out


def digest(items):
    # validators may correct lines/severity, so identity excludes them
    keyed = sorted(
        json.dumps({k: c.get(k) for k in ("defect_key", "file", "title", "claim")}, sort_keys=True)
        for c in items
    )
    return hashlib.sha256("\n".join(keyed).encode()).hexdigest()


def payload_hash(items):
    # the exact replayed input, unlike the correction-tolerant digest
    return hashlib.sha256(json.dumps(items, sort_keys=True).encode()).hexdigest()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def git_oid(repo, rev):
    r = subprocess.run(
        ["git", "-C", repo, "rev-parse", "--verify", f"{rev}^{{commit}}"],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        sys.exit(f"cannot resolve {rev!r} in {repo}: {r.stderr.strip()}")
    return r.stdout.strip()


def pin_validator(cfg, arm):
    """Make the arm's effective validator explicit before the investigator
    is replaced, so an inherited validator keeps the arm's model."""
    models = cfg.setdefault("models", {})
    route = models.get("validator") or models.get("investigator")
    if not isinstance(route, dict):
        sys.exit(f"{arm}: needs models.investigator or models.validator")
    models["validator"] = copy.deepcopy(route)
    return models["validator"]


def expected_model(route):
    # scripted routes record "scripted" as their model in the ledger
    return "scripted" if route.get("protocol") == "scripted" else route.get("model")


def route_summary(route):
    return {
        k: route.get(k)
        for k in (
            "protocol",
            "base_url",
            "model",
            "reasoning",
            "max_output_tokens",
            "temperature",
            "cache",
        )
        if k in route
    }


def controlled(cfg):
    """The arm config with everything the comparison varies removed."""
    c = copy.deepcopy(cfg)
    c.get("models", {}).pop("validator", None)
    c.get("models", {}).pop("investigator", None)
    return c


def diff_keys(a, b, prefix=""):
    if isinstance(a, dict) and isinstance(b, dict):
        out = []
        for k in sorted(set(a) | set(b)):
            out += diff_keys(a.get(k), b.get(k), f"{prefix}{k}.")
        return out
    return [] if a == b else [prefix.rstrip(".")]


def score(findings, truth, min_sev, candidate_fixes):
    """Per-arm scores against external truth labels. Uncertain verdicts and
    validator failures are counted separately, never as rejections."""
    s = {
        k: 0
        for k in (
            "true_accepted",
            "false_rejected",
            "true_rejected",
            "false_accepted",
            "true_uncertain",
            "false_uncertain",
            "true_below_threshold",
            "failures",
            "high_impact_missed",
            "unlabeled",
            "fix_published",
            "unsafe_fix_published",
        )
    }
    cases = []
    for f in findings:
        label = truth.get(f.get("defect_key"))
        st = f.get("validation_status")
        rationale = f.get("rationale") or ""
        failed = st == "uncertain" and (
            rationale.startswith("validator unavailable")
            or rationale.startswith("validator returned malformed")
            or rationale == "run time budget exhausted"
        )
        surfaced = (
            st == "accepted" and SEVERITY_RANK.get(f.get("severity"), -1) >= SEVERITY_RANK[min_sev]
        )
        fix = f.get("validated_fix")
        cases.append(
            {
                "defect_key": f.get("defect_key"),
                "status": st,
                "failed": failed,
                "severity": f.get("severity"),
                "start_line": f.get("start_line"),
                "surfaced": surfaced,
                "validated_fix": fix,
                "label": label.get("label") if label else None,
            }
        )
        if label is None:
            s["unlabeled"] += 1
            continue
        true = label.get("label") == "true"
        if failed:
            s["failures"] += 1
        elif st == "uncertain":
            s["true_uncertain" if true else "false_uncertain"] += 1
        elif st == "accepted" and not surfaced:
            s["true_below_threshold" if true else "false_rejected"] += 1
        elif st == "accepted":
            s["true_accepted" if true else "false_accepted"] += 1
        else:
            s["true_rejected" if true else "false_rejected"] += 1
        if true and label.get("high_impact") and not surfaced:
            s["high_impact_missed"] += 1
        if surfaced and fix:
            s["fix_published"] += 1
            if (
                label.get("fix_safe") is False
                and fix.strip() == (candidate_fixes.get(f.get("defect_key")) or "").strip()
            ):
                s["unsafe_fix_published"] += 1
    return s, cases


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--candidates", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--base", required=True)
    ap.add_argument("--head", default="HEAD")
    ap.add_argument("--out", required=True)
    ap.add_argument("--truth")
    ap.add_argument(
        "--allow-config-diff",
        action="store_true",
        help="record, instead of refusing, non-validator config differences",
    )
    ap.add_argument(
        "--bin", default=os.path.join(os.path.dirname(__file__), "..", "target", "debug", "revera")
    )
    ap.add_argument("arms", nargs="+")
    a = ap.parse_args()

    frozen = candidates(json.load(open(a.candidates)))
    candidate_fixes = {c["defect_key"]: c.get("suggested_fix", "") for c in frozen}
    want = digest(frozen)
    payload = payload_hash(frozen)
    truth = json.load(open(a.truth)) if a.truth else None
    if truth is not None:
        bad = [k for k, v in truth.items() if v.get("label") not in ("true", "false")]
        if bad:
            sys.exit(f"truth labels must be 'true' or 'false': {bad}")
    a.out = os.path.abspath(a.out)
    src = os.path.realpath(a.repo)
    base_oid, head_oid = git_oid(src, a.base), git_oid(src, a.head)

    names = [os.path.splitext(os.path.basename(arm))[0] for arm in a.arms]
    dupes = sorted({n for n in names if names.count(n) > 1})
    if dupes:
        # arm names key the copied repo, config, report and log paths
        sys.exit(f"duplicate arm names {dupes}: give each arm config a distinct file name")

    # prepare every arm before running any, so config errors cost no calls
    arms = []
    for name, arm in zip(names, a.arms, strict=True):
        cfg = yaml.safe_load(open(arm)) or {}
        validator = pin_validator(cfg, arm)
        review = cfg.setdefault("review", {})
        # the threshold would drop validator-downgraded candidates from the
        # report and break the identity check; it is applied when scoring
        min_sev = str(review.get("min_severity", "low")).lower()
        if min_sev not in SEVERITY_RANK:
            sys.exit(f"{arm}: unknown review.min_severity {min_sev!r}")
        cfg.pop("profiles", None)
        arms.append((name, arm, cfg, validator, min_sev))

    first = controlled(arms[0][2])
    config_diff = {}
    for name, _, cfg, _, _ in arms[1:]:
        d = diff_keys(first, controlled(cfg))
        if d:
            config_diff[name] = d
    if config_diff and not a.allow_config_diff:
        sys.exit(
            f"arms differ outside the validator route: {config_diff} "
            "(pass --allow-config-diff to record instead)"
        )

    os.makedirs(a.out, exist_ok=True)
    script = os.path.abspath(os.path.join(a.out, "frozen-investigator.json"))
    json.dump(
        {
            "roles": {
                "investigator": [
                    [
                        {
                            "tool_calls": [
                                {
                                    "name": "submit_findings",
                                    "arguments": {
                                        "findings": frozen,
                                        "coverage": "frozen candidates",
                                    },
                                }
                            ]
                        }
                    ]
                ]
            }
        },
        open(script, "w"),
    )

    version = subprocess.run([a.bin, "--version"], capture_output=True, text=True).stdout.strip()
    prompts_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "prompts")
    provenance = {
        "base": base_oid,
        "head": head_oid,
        "binary": {"version": version, "sha256": sha256_file(a.bin)},
        "prompts_source_sha256": hashlib.sha256(
            b"".join(
                open(os.path.join(prompts_dir, p), "rb").read() + b"\0"
                for p in sorted(os.listdir(prompts_dir))
                if p.endswith(".md")
            )
        ).hexdigest()
        if os.path.isdir(prompts_dir)
        else None,
        "candidates": len(frozen),
        "candidate_digest": want,
        "candidate_payload_sha256": payload,
        "truth_sha256": sha256_file(a.truth) if a.truth else None,
        "config_differences": config_diff,
        "arms": {},
    }

    rows, ok = [], True
    for name, arm, cfg, validator, min_sev in arms:
        cfg["models"]["investigator"] = {
            "protocol": "scripted",
            "script": script,
            "model": "frozen",
        }
        cfg["review"].update(
            {"validate": True, "publish": "dry-run", "strategy": "baseline", "min_severity": "low"}
        )
        arm_cfg = os.path.join(a.out, f"{name}.config.yaml")
        yaml.safe_dump(cfg, open(arm_cfg, "w"), sort_keys=False)
        provenance["arms"][name] = {
            "source": os.path.abspath(arm),
            "config_sha256": sha256_file(arm_cfg),
            "validator": route_summary(validator),
            "budget": cfg.get("budget", {}),
            "concurrency": cfg["review"].get("concurrency"),
            "vera": {
                k: v for k, v in (cfg.get("vera") or {}).items() if k in ("enabled", "backend")
            },
        }
        report_path = os.path.join(a.out, f"{name}.report.json")
        # each arm reviews its own copy without prior state, so no arm sees
        # another arm's verdicts (rechecks would otherwise consume sessions)
        repo = os.path.join(a.out, f"{name}.repo")
        real_src, real_repo = os.path.realpath(src), os.path.realpath(repo)
        # --out inside --repo is fine; --repo inside the arm copy would be deleted
        if os.path.commonpath([real_src, real_repo]) == real_repo:
            sys.exit(f"--repo lies inside arm copy {repo}; choose another --out")
        shutil.rmtree(repo, ignore_errors=True)

        def skip_out(d, entries):
            # --out may live inside --repo; never copy it into itself
            return [
                n
                for n in entries
                if os.path.realpath(os.path.join(d, n)) == os.path.realpath(a.out)
            ]

        shutil.copytree(src, repo, symlinks=True, ignore=skip_out)
        state = os.path.join(repo, ".revera", "state.json")
        if os.path.lexists(state):
            os.remove(state)
        rc = subprocess.run(
            [
                a.bin,
                "review",
                "--repo",
                repo,
                "--base",
                base_oid,
                "--head",
                head_oid,
                "--config",
                arm_cfg,
                "--force",
                "--out",
                report_path,
            ],
            stdout=subprocess.DEVNULL,
            stderr=open(os.path.join(a.out, f"{name}.stderr.log"), "w"),
        ).returncode
        if not os.path.exists(report_path):
            rows.append({"arm": name, "status": "failed", "exit": rc})
            ok = False
            continue
        rep = json.load(open(report_path))
        st = rep.get("stats", {})
        got = digest(rep["findings"])
        by_route = rep.get("ledger", {}).get("by_route", [])
        # every candidate gets its own validator session, so validator
        # requests can never be fewer than candidates
        vreq = sum(r.get("requests", 0) for r in by_route if r.get("role") == "validator")
        vmodels = sorted({r.get("model") for r in by_route if r.get("role") == "validator"})
        vprompt = sum(r.get("prompt_tokens", 0) for r in by_route if r.get("role") == "validator")
        vcached = sum(
            r.get("cached_prompt_tokens", 0) for r in by_route if r.get("role") == "validator"
        )
        row = {
            "arm": name,
            "status": rep["status"],
            "exit": rc,
            "candidates": st.get("candidates"),
            "accepted": st.get("accepted"),
            "rejected": st.get("rejected"),
            "uncertain": st.get("uncertain"),
            "rechecked": st.get("resolved", 0) + st.get("reopened", 0),
            "validation": st.get("validation"),
            "validator_requests": vreq,
            "validator_models": vmodels,
            "validator_prompt_tokens": vprompt,
            "validator_cache_hit_rate": round(vcached / vprompt, 3) if vprompt else None,
            "validate_s": (rep.get("timing", {}) or {}).get("validate_ms", 0) / 1000,
            "min_severity": min_sev,
            "accepted_at_min_severity": sum(
                1
                for f in rep["findings"]
                if f.get("validation_status") == "accepted"
                and SEVERITY_RANK.get(f.get("severity"), -1) >= SEVERITY_RANK[min_sev]
            ),
            "candidate_digest": got,
            "candidate_payload_sha256": payload,
            "identical_candidates": got == want and st.get("candidates") == len(frozen),
        }
        if truth is not None:
            row["score"], cases = score(rep["findings"], truth, min_sev, candidate_fixes)
            json.dump(cases, open(os.path.join(a.out, f"{name}.cases.json"), "w"), indent=2)
        # validator errors make the run partial; explicit uncertain
        # verdicts keep it complete
        if (
            not row["identical_candidates"]
            or st.get("validation") != "fresh"
            or rep["status"] != "complete"
            or rc != 0
            or vreq < len(frozen)
            or row["rechecked"]
            or vmodels != [expected_model(validator)]
        ):
            ok = False
        rows.append(row)
        print(json.dumps(row))
    json.dump(
        {"provenance": provenance, "arms": rows},
        open(os.path.join(a.out, "summary.json"), "w"),
        indent=2,
    )
    if not ok:
        sys.exit(
            "frozen comparison invalid: arms did not validate the identical candidate set "
            "with their intended validator"
        )


if __name__ == "__main__":
    main()
