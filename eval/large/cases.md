# Large-repo multi-hop cases (lead-authored spec)

Upstream: https://github.com/BurntSushi/ripgrep pinned at commit
3fce3b5bb0236da2df6d99672afb8a719642eca7 (~50k LOC Rust, 11 crates).
Each case = fresh clone at the pin (tag `base`), one synthetic edit in ONE
file committed as `head`. The defect is only visible through code in OTHER
files/crates that is not touched by the diff. Truth lives in
eval/large/truth/<case>.json (never inside the repo).

## L1 linestep-terminator  (crates/searcher/src/lines.rs)
Commit message: "searcher: LineStep yields lines without their terminator"
Edit 1 — doc comment on `LineStep::next`:
  old: "    /// The range returned includes the line terminator. Ranges are always\n    /// non-empty."
  new: "    /// The range returned excludes the line terminator."
Edit 2 — in `next_impl`, the `Some(line_end)` arm:
  old:
            Some(line_end) => {
                let m = (self.pos, self.pos + line_end + 1);
                assert!(m.0 <= m.1);

                self.pos = m.1;
                Some(m)
            }
  new:
            Some(line_end) => {
                let m = (self.pos, self.pos + line_end);
                assert!(m.0 <= m.1);

                self.pos = m.1 + 1;
                Some(m)
            }
Why it is a defect: callers in crates/searcher/src/searcher/core.rs (~lines
260-340, 458) and glue.rs, plus crates/printer/src/standard.rs, treat the
returned ranges as contiguous, terminator-inclusive line spans (slicing
context, counting lines with lines::count over the span, trimming the
terminator themselves). Excluding the terminator drops bytes from output,
skews line-number accounting, and makes empty lines yield empty ranges.

## L2 printer-crlf  (crates/printer/src/util.rs)
Commit message: "printer: simplify trim_line_terminator"
Edit — in `trim_line_terminator`:
  old:
        let mut end = line.end() - 1;
        if lineterm.is_crlf() && end > 0 && buf.get(end - 1) == Some(&b'\r') {
            end -= 1;
        }
  new:
        let end = line.end() - 1;
Why: the searcher crate supports LineTerminator::CRLF (enabled by rg --crlf,
see crates/core/flags); with CRLF the printer now leaves a trailing `\r` on
every printed/replaced line (standard.rs, util.rs replace_all), corrupting
output and `--replace` results on Windows-style files.

## L3 globset-dot-ext  (crates/globset/src/pathutil.rs)
Commit message: "globset: a leading dot is not an extension"
Edit — in `file_name_ext`, after
        Some(i) => i,
    };
  insert:
    if last_dot_at == 0 {
        return None;
    }
Why: crates/globset/src/lib.rs ExtensionStrategy / RequiredExtensionStrategy
(and glob.rs `ext()` / `required_ext()`) short-circuit matching on
`candidate.ext` instead of the regex. Patterns like `*.env` or `**/*.rc`
now stop matching dotfiles such as `.env`, diverging from the regex semantics
(`*` matches empty) and from git's gitignore behavior. Affects rg --glob and
.gitignore handling (crates/ignore).

## L0 clean-without-terminator  (control, crates/searcher/src/lines.rs)
Commit message: "searcher: use ends_with in without_terminator"
Edit — body of `without_terminator`:
  old:
    let line_term = line_term.as_bytes();
    let start = bytes.len().saturating_sub(line_term.len());
    if bytes.get(start..) == Some(line_term) {
        return &bytes[..bytes.len() - line_term.len()];
    }
    bytes
  new:
    let line_term = line_term.as_bytes();
    if bytes.ends_with(line_term) {
        return &bytes[..bytes.len() - line_term.len()];
    }
    bytes
Behavior-preserving; any accepted finding is a false positive.
