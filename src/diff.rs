use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Add,
    Del,
    Ctx,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub status: FileStatus,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Default)]
pub struct DiffSet {
    pub files: Vec<FileDiff>,
}

impl DiffSet {
    pub fn file(&self, path: &str) -> Option<&FileDiff> {
        self.files
            .iter()
            .find(|f| f.new_path == path || f.old_path == path)
    }

    pub fn head_side_lines(&self, path: &str) -> BTreeSet<u32> {
        let mut out = BTreeSet::new();
        if let Some(f) = self.file(path) {
            for h in &f.hunks {
                for l in &h.lines {
                    if matches!(l.kind, DiffLineKind::Add | DiffLineKind::Ctx) {
                        if let Some(n) = l.new_no {
                            out.insert(n);
                        }
                    }
                }
            }
        }
        out
    }

    /// Hunk containing head-side line `line`, rendered with +/-/space prefixes.
    pub fn hunk_containing(&self, path: &str, line: u32) -> Option<String> {
        let f = self.file(path)?;
        for h in &f.hunks {
            let in_range = h
                .lines
                .iter()
                .any(|l| l.new_no == Some(line) && l.kind != DiffLineKind::Del);
            if in_range {
                let mut s = format!(
                    "@@ -{},{} +{},{} @@ {}\n",
                    h.old_start, h.old_len, h.new_start, h.new_len, f.new_path
                );
                for l in &h.lines {
                    let (pfx, nums) = match l.kind {
                        DiffLineKind::Add => ('+', format!("    ->{}", l.new_no.unwrap_or(0))),
                        DiffLineKind::Del => ('-', format!("{}->    ", l.old_no.unwrap_or(0))),
                        DiffLineKind::Ctx => (
                            ' ',
                            format!("{}:{}", l.old_no.unwrap_or(0), l.new_no.unwrap_or(0)),
                        ),
                    };
                    s.push_str(&format!("{} {:>12} {}\n", pfx, nums, l.text));
                }
                return Some(s);
            }
        }
        None
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        for f in &self.files {
            s.push_str(&format!("diff --git a/{} b/{}\n", f.old_path, f.new_path));
            for h in &f.hunks {
                s.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    h.old_start, h.old_len, h.new_start, h.new_len
                ));
                for l in &h.lines {
                    let p = match l.kind {
                        DiffLineKind::Add => '+',
                        DiffLineKind::Del => '-',
                        DiffLineKind::Ctx => ' ',
                    };
                    s.push(p);
                    s.push_str(&l.text);
                    s.push('\n');
                }
            }
        }
        s
    }

    /// Render truncated to `max_bytes` (per-file note when a file doesn't fit).
    pub fn render_truncated(&self, max_bytes: usize) -> String {
        let mut s = String::new();
        for f in &self.files {
            let mut part = format!("diff --git a/{} b/{}\n", f.old_path, f.new_path);
            for h in &f.hunks {
                part.push_str(&format!(
                    "@@ -{},{} +{},{} @@\n",
                    h.old_start, h.old_len, h.new_start, h.new_len
                ));
                for l in &h.lines {
                    let p = match l.kind {
                        DiffLineKind::Add => '+',
                        DiffLineKind::Del => '-',
                        DiffLineKind::Ctx => ' ',
                    };
                    part.push(p);
                    part.push_str(&l.text);
                    part.push('\n');
                }
            }
            if s.len() + part.len() > max_bytes {
                s.push_str(&format!(
                    "diff --git a/{} b/{}\n[file diff omitted: exceeds max_diff_bytes]\n",
                    f.old_path, f.new_path
                ));
            } else {
                s.push_str(&part);
            }
        }
        s
    }

    /// Per-file excerpt used for validator context.
    pub fn file_excerpt(&self, path: &str) -> String {
        let mut s = String::new();
        for f in &self.files {
            if f.new_path == path || f.old_path == path {
                s.push_str(&format!("diff --git a/{} b/{}\n", f.old_path, f.new_path));
                for h in &f.hunks {
                    s.push_str(&format!(
                        "@@ -{},{} +{},{} @@\n",
                        h.old_start, h.old_len, h.new_start, h.new_len
                    ));
                    for l in &h.lines {
                        let p = match l.kind {
                            DiffLineKind::Add => '+',
                            DiffLineKind::Del => '-',
                            DiffLineKind::Ctx => ' ',
                        };
                        s.push(p);
                        s.push_str(&l.text);
                        s.push('\n');
                    }
                }
            }
        }
        s
    }
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32, u32, u32)> {
    // @@ -a[,b] +c[,d] @@
    let line = line.trim_start_matches("@@");
    let mut it = line.split_whitespace();
    let old = it.next()?.strip_prefix('-')?;
    let new = it.next()?.strip_prefix('+')?;
    let (os, ol) = split_range(old);
    let (ns, nl) = split_range(new);
    Some((os, ol, ns, nl))
}

fn split_range(s: &str) -> (u32, u32) {
    match s.split_once(',') {
        Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
        None => (s.parse().unwrap_or(0), 1),
    }
}

fn strip_ab_prefix(path: &str) -> String {
    if let Some(p) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        p.to_string()
    } else {
        path.to_string()
    }
}

/// Parse `git diff` unified output (handles `diff --git`, rename headers,
/// new/deleted files, `\ No newline at end of file`).
pub fn parse_unified(text: &str) -> DiffSet {
    let mut set = DiffSet::default();
    let mut cur: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;
    let mut old_no = 0u32;
    let mut new_no = 0u32;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if let Some(mut f) = cur.take() {
                if let Some(h) = hunk.take() {
                    f.hunks.push(h);
                }
                set.files.push(f);
            }
            // diff --git a/x b/y
            let mut parts = rest.split_whitespace();
            let a = parts.next().map(strip_ab_prefix).unwrap_or_default();
            let b = parts.next().map(strip_ab_prefix).unwrap_or_default();
            cur = Some(FileDiff {
                old_path: a,
                new_path: b,
                status: FileStatus::Modified,
                hunks: vec![],
            });
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if let Some(p) = line.strip_prefix("rename from ") {
            f.old_path = p.to_string();
            continue;
        }
        if let Some(p) = line.strip_prefix("rename to ") {
            f.new_path = p.to_string();
            f.status = FileStatus::Renamed;
            continue;
        }
        if line.starts_with("new file mode") {
            f.status = FileStatus::Added;
            continue;
        }
        if line.starts_with("deleted file mode") {
            f.status = FileStatus::Deleted;
            continue;
        }
        if line.starts_with("--- ") {
            let p = line.trim_start_matches("-").trim();
            if p != "/dev/null" {
                f.old_path = strip_ab_prefix(p);
            } else {
                f.status = FileStatus::Added;
            }
            continue;
        }
        if line.starts_with("+++ ") {
            let p = line.trim_start_matches("+").trim();
            if p != "/dev/null" {
                f.new_path = strip_ab_prefix(p);
            } else {
                f.status = FileStatus::Deleted;
            }
            continue;
        }
        if line.starts_with("@@") {
            if let Some((os, ol, ns, nl)) = parse_hunk_header(line) {
                if let Some(h) = hunk.take() {
                    f.hunks.push(h);
                }
                old_no = os;
                new_no = ns;
                hunk = Some(Hunk {
                    old_start: os,
                    old_len: ol,
                    new_start: ns,
                    new_len: nl,
                    lines: vec![],
                });
            }
            continue;
        }
        if let Some(h) = hunk.as_mut() {
            if line.starts_with('\\') {
                // "\ No newline at end of file" — annotate previous line, skip.
                continue;
            }
            let (kind, text) = match line.as_bytes().first() {
                Some(b'+') => (DiffLineKind::Add, &line[1..]),
                Some(b'-') => (DiffLineKind::Del, &line[1..]),
                _ => (DiffLineKind::Ctx, line.strip_prefix(' ').unwrap_or(line)),
            };
            let (o, n) = match kind {
                DiffLineKind::Add => {
                    let r = (None, Some(new_no));
                    new_no += 1;
                    r
                }
                DiffLineKind::Del => {
                    let r = (Some(old_no), None);
                    old_no += 1;
                    r
                }
                DiffLineKind::Ctx => {
                    let r = (Some(old_no), Some(new_no));
                    old_no += 1;
                    new_no += 1;
                    r
                }
            };
            h.lines.push(DiffLine {
                kind,
                old_no: o,
                new_no: n,
                text: text.to_string(),
            });
        }
    }
    if let Some(f) = cur.take() {
        if let Some(h) = hunk.take() {
            let mut f = f;
            f.hunks.push(h);
            set.files.push(f);
            return set;
        }
        set.files.push(f);
    }
    set
}
