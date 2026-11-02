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
                    if matches!(l.kind, DiffLineKind::Add | DiffLineKind::Ctx)
                        && let Some(n) = l.new_no
                    {
                        out.insert(n);
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
                return Some(Self::render_hunk(f, h));
            }
        }
        None
    }

    /// Exact diff context, or the nearest head-side hunk within 40 lines.
    pub fn diff_context(&self, path: &str, line: u32) -> Result<String, String> {
        if let Some(context) = self.hunk_containing(path, line) {
            return Ok(context);
        }
        let f = self
            .file(path)
            .ok_or_else(|| format!("{path} is not changed in this pull request; use read_file"))?;
        let nearest = f.hunks.iter().min_by_key(|h| {
            let end = h.new_start.saturating_add(h.new_len.saturating_sub(1));
            h.new_start
                .saturating_sub(line)
                .max(line.saturating_sub(end))
        });
        if let Some(h) = nearest {
            let end = h.new_start.saturating_add(h.new_len.saturating_sub(1));
            let distance = h
                .new_start
                .saturating_sub(line)
                .max(line.saturating_sub(end));
            if distance <= 40 {
                return Ok(format!(
                    "Requested line {path}:{line} is outside the diff; showing head-side range {}.\n{}",
                    Self::head_range(h),
                    Self::render_hunk(f, h)
                ));
            }
        }
        let ranges = f
            .hunks
            .iter()
            .take(20)
            .map(Self::head_range)
            .collect::<Vec<_>>();
        let ranges = if ranges.is_empty() {
            "none".into()
        } else {
            ranges.join(", ")
        };
        let more = if f.hunks.len() > 20 { ", …" } else { "" };
        Err(format!(
            "no hunk within 40 lines of {path}:{line}; head-side hunk ranges: {ranges}{more}"
        ))
    }

    fn head_range(h: &Hunk) -> String {
        if h.new_len == 0 {
            // A deletion-only hunk has an anchor, not a head-side line range.
            format!("+{},0 (empty)", h.new_start)
        } else {
            format!(
                "+{}..{}",
                h.new_start,
                h.new_start.saturating_add(h.new_len - 1)
            )
        }
    }

    fn render_hunk(f: &FileDiff, h: &Hunk) -> String {
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
        s
    }

    pub fn render(&self) -> String {
        self.files.iter().map(Self::render_file).collect()
    }

    /// Files whose diff `render_truncated(max_bytes)` omits.
    pub fn omitted_files(&self, max_bytes: usize) -> Vec<String> {
        self.files
            .iter()
            .zip(self.selected_files(max_bytes))
            .filter(|(_, selected)| !selected)
            .map(|(f, _)| f.new_path.clone())
            .collect()
    }

    fn selected_files(&self, max_bytes: usize) -> Vec<bool> {
        let mut order: Vec<_> = (0..self.files.len()).collect();
        order.sort_by_key(|&i| file_priority(&self.files[i].new_path));
        let mut selected = vec![false; self.files.len()];
        let mut used = 0usize;
        for i in order {
            let f = &self.files[i];
            let bytes = Self::render_file(f).len();
            if used.saturating_add(bytes) <= max_bytes {
                selected[i] = true;
                used += bytes;
            } else {
                used = used.saturating_add(Self::omitted_note(f).len());
            }
        }
        selected
    }

    fn omitted_note(f: &FileDiff) -> String {
        format!(
            "diff --git a/{} b/{}\n[file diff omitted: exceeds max_diff_bytes; hunks still available through diff_context]\n",
            f.old_path, f.new_path
        )
    }

    /// Render truncated to `max_bytes` (per-file note when a file doesn't fit).
    pub fn render_truncated(&self, max_bytes: usize) -> String {
        let mut s = String::new();
        for (f, selected) in self.files.iter().zip(self.selected_files(max_bytes)) {
            s.push_str(&if selected {
                Self::render_file(f)
            } else {
                Self::omitted_note(f)
            });
        }
        s
    }

    fn render_file(f: &FileDiff) -> String {
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
        part
    }

    /// Per-file excerpt used for validator context.
    pub fn file_excerpt(&self, path: &str) -> String {
        self.files
            .iter()
            .filter(|f| f.new_path == path || f.old_path == path)
            .map(Self::render_file)
            .collect()
    }
}

/// Budget priority: source, tests, build/config, prose/guidance, then low-signal artifacts.
fn file_priority(path: &str) -> u8 {
    let path = path.to_ascii_lowercase();
    let parts: Vec<_> = path.split('/').collect();
    let name = parts.last().copied().unwrap_or("");
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    if matches!(extension, "lock" | "lockb")
        || matches!(
            name,
            "package-lock.json" | "npm-shrinkwrap.json" | "pnpm-lock.yaml" | "yarn.lock" | "go.sum"
        )
        || parts.iter().any(|part| {
            matches!(
                *part,
                "vendor"
                    | "vendors"
                    | "vendored"
                    | "third_party"
                    | "third-party"
                    | "node_modules"
                    | "generated"
                    | "__generated__"
                    | "gen"
                    | "dist"
            )
        })
        || name.contains(".min.")
        || name.contains(".generated.")
        || name.contains(".g.")
        || name.contains(".pb.")
        || name.ends_with("_pb2.py")
        || name.ends_with("_pb2_grpc.py")
    {
        return 4;
    }
    let build_config = matches!(
        extension,
        "toml"
            | "yaml"
            | "yml"
            | "json"
            | "jsonc"
            | "json5"
            | "ini"
            | "cfg"
            | "conf"
            | "config"
            | "xml"
            | "gradle"
            | "cmake"
    ) || parts.contains(&".github")
        || matches!(
            name,
            "makefile"
                | "gnumakefile"
                | "cmakelists.txt"
                | "build.rs"
                | "build"
                | "build.bazel"
                | "workspace"
                | "workspace.bazel"
                | "dockerfile"
                | "containerfile"
                | "justfile"
                | "gemfile"
                | "rakefile"
                | "setup.py"
                | ".gitignore"
                | ".gitattributes"
                | ".dockerignore"
                | ".editorconfig"
                | ".tool-versions"
        )
        || name.starts_with("dockerfile.");
    let source = matches!(
        extension,
        "rs" | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "hpp"
            | "hxx"
            | "cs"
            | "fs"
            | "fsx"
            | "go"
            | "java"
            | "kt"
            | "kts"
            | "scala"
            | "swift"
            | "m"
            | "mm"
            | "py"
            | "pyi"
            | "rb"
            | "php"
            | "pl"
            | "pm"
            | "lua"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "mjs"
            | "cjs"
            | "mts"
            | "cts"
            | "vue"
            | "svelte"
            | "html"
            | "htm"
            | "css"
            | "scss"
            | "sass"
            | "less"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "ps1"
            | "r"
            | "jl"
            | "ex"
            | "exs"
            | "erl"
            | "hrl"
            | "hs"
            | "lhs"
            | "clj"
            | "cljs"
            | "cljc"
            | "dart"
            | "sql"
            | "graphql"
            | "gql"
            | "proto"
            | "zig"
            | "ml"
            | "mli"
            | "nim"
    );
    if (matches!(
        extension,
        "md" | "markdown" | "mdx" | "rst" | "txt" | "adoc" | "asciidoc"
    ) && name != "cmakelists.txt")
        || matches!(
            name,
            "readme" | "license" | "copying" | "changelog" | "contributing" | "authors" | "notice"
        )
        || (!source
            && !build_config
            && parts.iter().any(|part| {
                matches!(
                    *part,
                    "doc"
                        | "docs"
                        | "documentation"
                        | "skill"
                        | "skills"
                        | "rule"
                        | "rules"
                        | ".agents"
                        | ".claude"
                        | ".opencode"
                        | ".cursor"
                )
            }))
    {
        return 3;
    }
    if parts.iter().any(|part| {
        matches!(
            *part,
            "test" | "tests" | "__tests__" | "spec" | "specs" | "fixtures" | "testdata"
        )
    }) || name.starts_with("test_")
        || name.contains(".test.")
        || name.contains(".spec.")
        || name
            .rsplit_once('.')
            .is_some_and(|(stem, _)| stem.ends_with("_test") || stem.ends_with("_tests"))
    {
        return 1;
    }
    if build_config {
        return 2;
    }
    0
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

/// Unquote a git C-style quoted path (`"a\tb"` → `a<TAB>b`, `\ooo` octal
/// escapes decoded to bytes then UTF-8).
fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let b = inner.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 1;
            match b[i] {
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                c @ b'0'..=b'7' => {
                    let mut v = (c - b'0') as u16;
                    let mut n = 1;
                    while n < 3 && i + 1 < b.len() && (b'0'..=b'7').contains(&b[i + 1]) {
                        i += 1;
                        v = v * 8 + (b[i] - b'0') as u16;
                        n += 1;
                    }
                    out.push(v as u8);
                }
                c => out.push(c),
            }
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned())
}

/// Next path token from a diff header line; handles quoted paths containing
/// spaces. Returns (decoded path, remainder).
fn path_token(s: &str) -> (String, &str) {
    let s = s.trim_start();
    if s.starts_with('"') {
        let b = s.as_bytes();
        let mut i = 1;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'"' => {
                    let tok = &s[..=i];
                    return (unquote(tok).unwrap_or_else(|| tok.to_string()), &s[i + 1..]);
                }
                _ => i += 1,
            }
        }
        (unquote(s).unwrap_or_else(|| s.to_string()), "")
    } else {
        match s.find(char::is_whitespace) {
            Some(i) => (s[..i].to_string(), &s[i..]),
            None => (s.to_string(), ""),
        }
    }
}

/// Unquote (if quoted) then strip a/ or b/ prefix.
fn normalize_diff_path(path: &str) -> String {
    let p = path.trim();
    let p = if p.starts_with('"') {
        unquote(p).unwrap_or_else(|| p.to_string())
    } else {
        p.to_string()
    };
    strip_ab_prefix(&p)
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
            // diff --git a/x b/y — paths may be C-quoted (spaces, non-ASCII)
            let (a_raw, rest) = path_token(rest);
            let (b_raw, _) = path_token(rest);
            let a = strip_ab_prefix(&a_raw);
            let b = strip_ab_prefix(&b_raw);
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
            f.old_path = normalize_diff_path(p);
            continue;
        }
        if let Some(p) = line.strip_prefix("rename to ") {
            f.new_path = normalize_diff_path(p);
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
                f.old_path = normalize_diff_path(p);
            } else {
                f.status = FileStatus::Added;
            }
            continue;
        }
        if line.starts_with("+++ ") {
            let p = line.trim_start_matches("+").trim();
            if p != "/dev/null" {
                f.new_path = normalize_diff_path(p);
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

#[cfg(test)]
mod tests {
    use super::file_priority;

    #[test]
    fn diff_file_priority_classifies_common_paths() {
        for (priority, paths) in [
            (
                0,
                vec![
                    "src/main.rs",
                    "app/page.tsx",
                    "lib/module.py",
                    "scripts/run.sh",
                    "src/rules/engine.rs",
                    "docs/src/App.tsx",
                    "docs/example.rs",
                    ".agents/skills/tool/run.py",
                    ".opencode/rules/rule.ts",
                    "skills/review/script.sh",
                ],
            ),
            (
                1,
                vec![
                    "tests/api.rs",
                    "src/parser_tests.rs",
                    "pkg/api_test.go",
                    "src/page.spec.ts",
                    "test_api.py",
                    "__tests__/app.ts",
                ],
            ),
            (
                2,
                vec![
                    "Cargo.toml",
                    "package.json",
                    ".github/workflows/ci.yml",
                    "Dockerfile",
                    "build.rs",
                    "CMakeLists.txt",
                    ".gitignore",
                    "docs/Cargo.toml",
                    "docs/CMakeLists.txt",
                    "docs/Dockerfile",
                    ".claude/settings.json",
                ],
            ),
            (
                3,
                vec![
                    "README.md",
                    "guide.rst",
                    "notes.txt",
                    "guide.adoc",
                    ".cursor/rules/rule.mdc",
                    ".claude/skills/x/SKILL.md",
                    "docs/guide.md",
                ],
            ),
            (
                4,
                vec![
                    "Cargo.lock",
                    "package-lock.json",
                    "pnpm-lock.yaml",
                    "go.sum",
                    "vendor/src/lib.rs",
                    "third_party/tests/api.rs",
                    "app.min.js",
                    "docs/generated/api.md",
                    "src/model.generated.rs",
                    "src/api.pb.go",
                ],
            ),
        ] {
            for path in paths {
                assert_eq!(file_priority(path), priority, "{path}");
            }
        }
    }
}
