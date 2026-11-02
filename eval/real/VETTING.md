# Vetting real-bug candidates

Instructions given to each vetter (one per language group) after `mine.py`.
Their output is checked and merged by `merge-vetted.py --write`.

You are vetting real-bug candidates for a code-review benchmark. Work only inside the revera checkout (read anything there; write ONLY the one output file named below). Do not write anywhere else. Do not use the network: every repository you need is already cloned.

Context: an SZZ-style miner (eval/real/mine.py) found pairs (intro commit, fix commit): a later commit `fix` changed lines that `intro` last touched. Candidates are JSON lines in eval/tmp/cand-<repo>.jsonl with keys intro, intro_subject, intro_lines, fix, fix_subject, fix_body, blamed (file, line in the intro commit). Mirrors: eval/tmp/mirrors/<repo>. Many candidates are NOT valid. The benchmark asks: "given only the intro commit's diff and the repository at that commit, could a careful reviewer have flagged this defect?"

Keep a candidate only if ALL hold (check with git, e.g. `git -C eval/tmp/mirrors/<repo> show <intro>`, `git show <fix>`, `git show <intro>^:<path>`):
1. The fix corrects a real behaviour defect (wrong result, crash, security, data loss, resource leak, wrong error), not a typo, doc, style, type-only, test-only, perf-only, refactor or feature request.
2. The intro commit INTRODUCED it: the faulty logic did not already exist in `<intro>^` (compare the parent version of the lines the fix changes). Moved, reformatted or equivalent code is NOT introduction. This is the most common failure: check it carefully.
3. It is reviewable from the intro diff: the faulty lines are in the intro diff (or the intro diff changes their meaning), and spotting it needs no knowledge of later requirements, upstream API changes, or platform quirks no reviewer could know.
4. intro diff is at most ~300 changed lines.

Prefer clear, varied defects over many similar ones. Pick at most 8 kept defects for your group. Also pick 2 presumed-clean commits from your repos: small (20-200 changed lines) non-merge source commits that are behaviour changes (feature or fix, not docs/tests/version bumps) and whose changed lines no later commit with fix/bug/regression in the subject modified (check with `git log --format='%h %s' <sha>..HEAD -L <start>,<end>:<path>` sparingly, or blame at HEAD).

For each kept defect, give the defect location in the INTRO commit's version of the file (head side of the intro diff): file path, line_min and line_max (a window of at most ~6 lines that contains the faulty line(s)), and 4-6 lowercase keywords a correct review comment would likely contain (mix of the symptom and the mechanism, e.g. ["empty string","falsy","!= null","dropped"]). Verify line numbers with `git show <intro>:<path> | sed -n 'A,Bp'`.

Write a JSON array to OUTPUT_PATH with objects:
{"name": "real-<repo>-<intro first 8 chars>" (clean ones: "real-clean-<repo>-<sha8>"), "repo": "<repo>", "url": "https://github.com/<owner>/<repo>", "intro": "<full sha>", "subject": "<intro subject>", "fix": "<full fix sha>", "fix_subject": "...", "defects": [{"file": "...", "line_min": N, "line_max": M, "keywords": [...]}], "why": "one or two sentences: what the bug is and why it was introduced by intro (what the parent did instead)"}
Clean commits: same keys but "fix": "", "fix_subject": "", "defects": [], "clean": true, "why": "why it is presumed clean".

Your group: <repos with owners>. OUTPUT_PATH: eval/tmp/vet-<group>.json
Finish by replying with a short table of kept and rejected counts per repo and the reason categories for rejections.
