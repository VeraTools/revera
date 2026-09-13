use crate::diff::DiffSet;
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

/// Anchor accepted findings: inline when the file is in the diff and
/// start_line is a head-side hunk line; else summary. Inline comments are
/// capped at `max_findings`, ordered by severity then file/line.
pub fn anchor(diff: &DiffSet, findings: Vec<Finding>, max_findings: usize) -> Vec<Anchored> {
    let mut items: Vec<Anchored> = findings
        .into_iter()
        .map(|f| {
            let mut f = f;
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
