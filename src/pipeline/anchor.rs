use crate::diff::{DiffLineKind, DiffSet};
use crate::findings::{Finding, ValidationStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    Inline,
    Summary,
}

#[derive(Debug, Clone)]
pub struct Anchored {
    pub finding: Finding,
    pub placement: Placement,
}

/// Where a quote sits on the head side of a file's diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteMatch {
    /// Exactly one hunk window matches: the head-side line range.
    Unique(u32, u32),
    Ambiguous,
    NotFound,
}

/// Quote lines compared by content: surrounding whitespace, an optional
/// leading `+` diff marker and blank lines are ignored.
fn normalize_quote(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            let t = l.trim();
            t.strip_prefix('+').map(str::trim).unwrap_or(t).to_string()
        })
        .filter(|l| !l.is_empty())
        .collect()
}

/// Match `quote` against the head-side (added and context) lines of each hunk
/// of `file`. A match must lie inside one hunk; deleted lines never match, so
/// a quote can only anchor where GitHub can place a RIGHT-side comment.
pub fn resolve_quote(diff: &DiffSet, file: &str, quote: &str) -> QuoteMatch {
    let want = normalize_quote(quote);
    let Some(fd) = diff.file(file) else {
        return QuoteMatch::NotFound;
    };
    if want.is_empty() {
        return QuoteMatch::NotFound;
    }
    let mut found: Option<(u32, u32)> = None;
    for h in &fd.hunks {
        let head: Vec<(u32, String)> = h
            .lines
            .iter()
            .filter(|l| l.kind != DiffLineKind::Del)
            .filter_map(|l| {
                let t = l.text.trim();
                let n = l.new_no?;
                (!t.is_empty()).then(|| (n, t.to_string()))
            })
            .collect();
        for w in head.windows(want.len()) {
            if w.iter().zip(&want).all(|((_, a), b)| a == b) {
                if found.is_some() {
                    return QuoteMatch::Ambiguous;
                }
                found = Some((w[0].0, w[w.len() - 1].0));
            }
        }
    }
    match found {
        Some((s, e)) => QuoteMatch::Unique(s, e),
        None => QuoteMatch::NotFound,
    }
}

/// Anchor accepted findings: inline when the file is in the diff and
/// start_line is a head-side hunk line; else summary. Inline comments are
/// capped at `max_findings`, ordered by severity then file/line.
pub fn anchor(diff: &DiffSet, findings: Vec<Finding>, max_findings: usize) -> Vec<Anchored> {
    let mut items: Vec<Anchored> = findings
        .into_iter()
        .map(|f| {
            let mut f = f;
            // a quote overrides counted line numbers; a quote that does not
            // match exactly once cannot be placed inline honestly
            if let Some(q) = f.quoted_code.clone() {
                match resolve_quote(diff, &f.file, &q) {
                    QuoteMatch::Unique(start, end) => {
                        f.start_line = start;
                        f.end_line = (end > start).then_some(end);
                        f.quote_anchored = true;
                    }
                    QuoteMatch::Ambiguous | QuoteMatch::NotFound => {
                        return Anchored {
                            finding: f,
                            placement: Placement::Summary,
                        };
                    }
                }
            }
            // drop an invalid end_line: it must be >= start_line and a
            // head-side diff line; a bad range becomes a single-line comment
            if let Some(end) = f.end_line {
                let ok = end >= f.start_line
                    && diff
                        .file(&f.file)
                        .map(|_| diff.head_side_lines(&f.file).contains(&end))
                        .unwrap_or(false);
                if !ok {
                    f.end_line = None;
                }
            }
            let inline_ok = diff.file(&f.file).is_some()
                && diff.head_side_lines(&f.file).contains(&f.start_line);
            Anchored {
                finding: f,
                placement: if inline_ok {
                    Placement::Inline
                } else {
                    Placement::Summary
                },
            }
        })
        .collect();
    // cap inline: sort eligible by severity desc then file/line, overflow -> summary
    let mut inline_idx: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, a)| a.placement == Placement::Inline)
        .map(|(i, _)| i)
        .collect();
    inline_idx.sort_by(|&a, &b| {
        let fa = &items[a].finding;
        let fb = &items[b].finding;
        fb.severity
            .cmp(&fa.severity)
            .then(fa.file.cmp(&fb.file))
            .then(fa.start_line.cmp(&fb.start_line))
    });
    for (rank, idx) in inline_idx.into_iter().enumerate() {
        if rank >= max_findings {
            items[idx].placement = Placement::Summary;
        }
    }
    items
}

pub fn is_publishable(f: &Finding) -> bool {
    f.validation_status == Some(ValidationStatus::Accepted)
}
