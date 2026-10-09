use crate::diff::DiffSet;
use crate::provider::ToolSpec;
use crate::vera::VeraClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;

/// Per-call cap for local git-backed tools.
const LOCAL_TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Lightweight per-tool telemetry: counts, errors, and cumulative latency.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolStat {
    pub name: String,
    pub calls: u64,
    pub errors: u64,
    pub latency_ms: u64,
}

#[derive(Default)]
struct Stats {
    by_tool: BTreeMap<String, ToolStat>,
    /// distinct repository files read via `read_file`
    files_read: std::collections::BTreeSet<String>,
}

pub struct ToolBox {
    pub repo_root: PathBuf,
    pub diff: Arc<DiffSet>,
    pub vera: Arc<VeraClient>,
    pub max_output_bytes: usize,
    vera_disabled: Mutex<Option<String>>,
    /// vera.enabled=false: vera_* tools are not offered at all.
    hide_vera: bool,
    /// When set, only these tool names are exposed/callable.
    allowed: Option<Vec<String>>,
    stats: Arc<Mutex<Stats>>,
    /// Reviewed head commit: file reads come from this immutable tree
    /// (tracked regular files only) rather than the working directory.
    head: Option<String>,
    /// Corpus policy shared with Vera indexing and lexical tools.
    excluded: globset::GlobSet,
    /// Vera indexes the working tree, so hits may include paths absent at head.
    head_files: Arc<OnceCell<HashSet<String>>>,
}

/// Largest file `read_file` will load.
const MAX_READ_BYTES: usize = 4 * 1024 * 1024;

pub fn glob_set<S: AsRef<str>>(globs: &[S]) -> globset::GlobSet {
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        let g = g.as_ref();
        match globset::GlobBuilder::new(g)
            .literal_separator(false)
            .build()
        {
            Ok(gl) => {
                b.add(gl);
            }
            Err(e) => tracing::warn!("ignoring invalid exclude glob {g:?}: {e}"),
        }
    }
    b.build().unwrap_or_else(|_| globset::GlobSet::empty())
}

fn obj_schema(props: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
    })
}

pub fn terminal_submit_findings_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_findings".into(),
        description: "Submit the final candidate findings and finish. Call exactly once.".into(),
        parameters: obj_schema(
            json!({
                "findings": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "defect_key": {"type": "string"},
                        "severity": {"type": "string", "enum": ["high","medium","low"]},
                        "file": {"type": "string"},
                        "start_line": {"type": "integer"},
                        "end_line": {"type": "integer"},
                        "title": {"type": "string"},
                        "claim": {"type": "string"},
                        "trigger": {"type": "string"},
                        "impact": {"type": "string"},
                        "introduced_by_change": {"type": "boolean"},
                        "supporting_evidence": {"type": "array", "items": {"type": "object",
                            "properties": {"path": {"type": "string"}, "start_line": {"type": "integer"}, "end_line": {"type": "integer"}, "note": {"type": "string"}},
                            "required": ["path"]}},
                        "counterevidence_checked": {"type": "array", "items": {"type": "string"}},
                        "suggested_fix": {"type": "string"},
                    },
                    "required": ["defect_key","severity","file","start_line","title","claim"],
                }},
                "coverage": {"type": "string"},
                "not_checked": {"type": "array", "items": {"type": "string"}},
            }),
            &["findings", "coverage"],
        ),
    }
}

/// Delegated-mode lead planning terminal tool.
pub fn terminal_submit_plan_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_plan".into(),
        description: "Submit the investigation plan. Call exactly once.".into(),
        parameters: obj_schema(
            json!({
                "questions": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "question": {"type": "string"},
                        "symbols": {"type": "array", "items": {"type": "string"}},
                        "expected_evidence": {"type": "string"},
                        "stop_condition": {"type": "string"},
                        "files_hint": {"type": "array", "items": {"type": "string"}},
                    },
                    "required": ["id","question","symbols","expected_evidence","stop_condition"],
                }},
                "note": {"type": "string"},
            }),
            &["questions"],
        ),
    }
}

/// Delegated-mode worker terminal tool.
pub fn terminal_submit_worker_result_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_worker_result".into(),
        description: "Submit your answer to the assigned question. Call exactly once.".into(),
        parameters: obj_schema(
            json!({
                "result": {"type": "string", "enum": ["answered","blocked","no_issue","complete"]},
                "blocked_reason": {"type": "string"},
                "answer": {"type": "string"},
                "evidence": {"type": "array", "items": {"type": "object",
                    "properties": {"path": {"type": "string"}, "start_line": {"type": "integer"}, "end_line": {"type": "integer"}, "note": {"type": "string"}},
                    "required": ["path"]}},
                "candidate_findings": {"type": "array", "items": {"type": "object"}},
                "gaps": {"type": "array", "items": {"type": "string"}},
            }),
            &["result", "answer"],
        ),
    }
}

pub fn terminal_submit_verdict_spec() -> ToolSpec {
    ToolSpec {
        name: "submit_verdict".into(),
        description: "Submit the validation verdict for one candidate finding. Call exactly once."
            .into(),
        parameters: obj_schema(
            json!({
                "validation_status": {"type": "string", "enum": ["accepted","rejected","uncertain"]},
                "counterevidence_checked": {"type": "array", "items": {"type": "string"}},
                "severity": {"type": "string", "enum": ["high","medium","low"]},
                "start_line": {"type": "integer"},
                "end_line": {"type": "integer"},
                "rationale": {"type": "string"},
                "fix": {"type": "string", "description": "Remedy you verified against the code; omit unless you checked it"},
            }),
            &["validation_status", "rationale"],
        ),
    }
}

impl ToolBox {
    pub fn new(
        repo_root: PathBuf,
        diff: Arc<DiffSet>,
        vera: Arc<VeraClient>,
        max_output_bytes: usize,
    ) -> Self {
        let excluded = glob_set(&vera.exclude);
        Self {
            repo_root,
            diff,
            vera,
            max_output_bytes,
            vera_disabled: Mutex::new(None),
            hide_vera: false,
            allowed: None,
            stats: Arc::new(Mutex::new(Stats::default())),
            head: None,
            excluded,
            head_files: Arc::new(OnceCell::new()),
        }
    }

    /// Serve file reads from the tracked tree of `head` (a full object id).
    pub fn with_head(mut self, head: &str) -> Self {
        self.head = Some(head.to_string());
        self
    }

    /// Whether the content policy hides `path` from every model surface.
    pub fn is_excluded(&self, path: &str) -> bool {
        Self::is_internal_path(path) || self.excluded.is_match(path)
    }

    /// Snapshot of per-tool telemetry, sorted by tool name.
    pub fn tool_stats(&self) -> Vec<ToolStat> {
        self.stats
            .lock()
            .unwrap()
            .by_tool
            .values()
            .cloned()
            .collect()
    }

    /// Number of distinct files read with `read_file`.
    pub fn files_read(&self) -> usize {
        self.stats.lock().unwrap().files_read.len()
    }

    /// Reason vera_* tools are currently unavailable, if any.
    pub fn vera_unavailable(&self) -> Option<String> {
        if self.hide_vera {
            return Some("vera disabled by config".into());
        }
        self.vera_disabled.lock().unwrap().clone()
    }

    /// A toolbox exposing only the named tools (e.g. synthesis gets read-only
    /// file access and nothing else).
    pub fn restricted(&self, names: &[&str]) -> ToolBox {
        ToolBox {
            repo_root: self.repo_root.clone(),
            diff: self.diff.clone(),
            vera: self.vera.clone(),
            max_output_bytes: self.max_output_bytes,
            vera_disabled: Mutex::new(self.vera_disabled.lock().unwrap().clone()),
            hide_vera: self.hide_vera,
            allowed: Some(names.iter().map(|s| s.to_string()).collect()),
            stats: self.stats.clone(),
            head: self.head.clone(),
            excluded: self.excluded.clone(),
            head_files: self.head_files.clone(),
        }
    }

    /// After index/retrieval setup fails, vera_* tools fail fast instead of
    /// re-invoking vera on every call.
    pub fn disable_vera(&self, reason: String) {
        *self.vera_disabled.lock().unwrap() = Some(reason);
    }

    /// vera.enabled=false: drop vera_* tools from specs entirely.
    pub fn hide_vera_tools(&mut self) {
        self.hide_vera = true;
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut all: Vec<ToolSpec> = vec![
            ToolSpec {
                name: "read_file".into(),
                description:
                    "Read numbered lines of a file at the PR head. Max 400 lines per call.".into(),
                parameters: obj_schema(
                    json!({
                        "path": {"type": "string"},
                        "start_line": {"type": "integer"},
                        "end_line": {"type": "integer"},
                    }),
                    &["path"],
                ),
            },
            ToolSpec {
                name: "list_changed_files".into(),
                description: "List files changed in this PR with status and +/- counts.".into(),
                parameters: obj_schema(json!({}), &[]),
            },
            ToolSpec {
                name: "diff_context".into(),
                description: "Show the diff hunk containing a head-side line of a changed file."
                    .into(),
                parameters: obj_schema(
                    json!({
                        "path": {"type": "string"},
                        "line": {"type": "integer"},
                    }),
                    &["path", "line"],
                ),
            },
            ToolSpec {
                name: "grep_repo".into(),
                description: "Regex (ERE) search over all tracked files at the PR head, independent of the semantic index. Use to find callers, usages and definitions anywhere in the repository. Output: path:line:text.".into(),
                parameters: obj_schema(
                    json!({
                        "pattern": {"type": "string"},
                        "path_glob": {"type": "string", "description": "optional glob, e.g. src/**/*.rs"},
                        "limit": {"type": "integer", "description": "max matching lines (default 40, max 200)"},
                    }),
                    &["pattern"],
                ),
            },
            ToolSpec {
                name: "find_files".into(),
                description: "List tracked files at the PR head matching a glob (e.g. **/*handler*.go). Max 200 results.".into(),
                parameters: obj_schema(
                    json!({
                        "glob": {"type": "string"},
                    }),
                    &["glob"],
                ),
            },
            ToolSpec {
                name: "vera_search".into(),
                description: "Semantic/keyword search over the indexed repository.".into(),
                parameters: obj_schema(
                    json!({
                        "query": {"type": "string"},
                        "intent": {"type": "string"},
                        "path_glob": {"type": "string"},
                        "lang": {"type": "string"},
                        "limit": {"type": "integer"},
                    }),
                    &["query"],
                ),
            },
            ToolSpec {
                name: "vera_references".into(),
                description:
                    "Find callers (default) or callees of a symbol via the Vera call graph.".into(),
                parameters: obj_schema(
                    json!({
                        "symbol": {"type": "string"},
                        "direction": {"type": "string", "enum": ["callers","callees"]},
                        "limit": {"type": "integer"},
                    }),
                    &["symbol"],
                ),
            },
            ToolSpec {
                name: "vera_grep".into(),
                description: "Regex search over indexed files.".into(),
                parameters: obj_schema(
                    json!({
                        "pattern": {"type": "string"},
                        "path_glob": {"type": "string"},
                        "limit": {"type": "integer"},
                    }),
                    &["pattern"],
                ),
            },
            ToolSpec {
                name: "vera_overview".into(),
                description: "High-level architecture summary of the indexed repository.".into(),
                parameters: obj_schema(json!({}), &[]),
            },
        ];
        if self.hide_vera {
            all.retain(|t| !t.name.starts_with("vera_"));
        }
        match &self.allowed {
            Some(names) => all
                .into_iter()
                .filter(|t| names.contains(&t.name))
                .collect(),
            None => all,
        }
    }

    fn truncate(&self, s: String) -> String {
        crate::text::truncate_bytes(&s, self.max_output_bytes)
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf, String> {
        let p = PathBuf::from(path);
        if p.is_absolute() || path.contains("..") {
            return Err("path must be relative to the repo root".into());
        }
        let joined = self.repo_root.join(&p);
        let canon = joined
            .canonicalize()
            .map_err(|e| format!("cannot resolve {path}: {e}"))?;
        if !canon.starts_with(&self.repo_root) {
            return Err("path escapes repo root".into());
        }
        for comp in canon.strip_prefix(&self.repo_root).unwrap().components() {
            let s = comp.as_os_str().to_string_lossy();
            if Self::is_internal_dir(&s) {
                return Err(format!("path under {s} is not readable"));
            }
        }
        Ok(canon)
    }

    async fn read_file(&self, args: &Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("missing path")?;
        let path = path.trim_start_matches("./");
        if self.is_excluded(path) {
            return Err(format!(
                "{path} is excluded by the repository content policy"
            ));
        }
        let text = match &self.head {
            Some(head) => {
                if path.is_empty() || path.starts_with('/') || path.split('/').any(|c| c == "..") {
                    return Err("path must be relative to the repo root".into());
                }
                let bytes = crate::git::read_blob(&self.repo_root, head, path, MAX_READ_BYTES)
                    .await
                    .map_err(|e| format!("read {path}: {e}"))?
                    .ok_or_else(|| {
                        format!("{path} is not a tracked regular file at the reviewed head")
                    })?;
                String::from_utf8(bytes).map_err(|_| format!("{path} is not UTF-8 text"))?
            }
            None => {
                let canon = self.resolve_path(path)?;
                std::fs::read_to_string(&canon).map_err(|e| format!("read {path}: {e}"))?
            }
        };
        self.stats
            .lock()
            .unwrap()
            .files_read
            .insert(path.to_string());
        let start = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
        let end = args["end_line"].as_u64().map(|e| e as usize);
        let lines: Vec<&str> = text.lines().collect();
        let mut out = String::new();
        let mut n = 0usize;
        for (i, l) in lines.iter().enumerate() {
            let ln = i + 1;
            if ln < start {
                continue;
            }
            if let Some(e) = end
                && ln > e
            {
                break;
            }
            if n >= 400 {
                out.push_str("...[max 400 lines per call]\n");
                break;
            }
            out.push_str(&format!("{}| {}\n", ln, l));
            n += 1;
        }
        Ok(out)
    }

    /// Git, Revera and Vera working directories (Vera 2 builds into
    /// `.vera.build` and keeps failed-index checkpoints in `.vera.resume`).
    fn is_internal_dir(c: &str) -> bool {
        matches!(
            c,
            ".git" | ".vera" | ".vera.build" | ".vera.resume" | ".revera"
        )
    }

    fn is_internal_path(p: &str) -> bool {
        p.split('/').any(Self::is_internal_dir)
    }

    async fn grep_repo(&self, args: &Value) -> Result<String, String> {
        let pat = args["pattern"].as_str().ok_or("missing pattern")?;
        if pat.trim().is_empty() {
            return Err("pattern must not be empty".into());
        }
        let limit = args["limit"].as_u64().unwrap_or(40).clamp(1, 200) as usize;
        let out = tokio::time::timeout(
            LOCAL_TOOL_TIMEOUT,
            crate::git::grep(
                &self.repo_root,
                pat,
                args["path_glob"].as_str(),
                &self.vera.exclude,
            ),
        )
        .await
        .map_err(|_| "grep_repo timed out".to_string())?
        .map_err(|e| e.to_string())?;
        let mut lines: Vec<&str> = out
            .lines()
            .filter(|l| !Self::is_internal_path(l.split(':').next().unwrap_or("")))
            .collect();
        let total = lines.len();
        lines.truncate(limit);
        if lines.is_empty() {
            return Ok("no matches".into());
        }
        let mut s = lines.join("\n");
        s.push('\n');
        if total > limit {
            s.push_str(&format!(
                "...[{} more matching lines; narrow the pattern or path_glob]\n",
                total - limit
            ));
        }
        Ok(s)
    }

    async fn find_files(&self, args: &Value) -> Result<String, String> {
        let glob = args["glob"].as_str().ok_or("missing glob")?;
        let files = tokio::time::timeout(
            LOCAL_TOOL_TIMEOUT,
            crate::git::ls_files(&self.repo_root, Some(glob), &self.vera.exclude),
        )
        .await
        .map_err(|_| "find_files timed out".to_string())?
        .map_err(|e| e.to_string())?;
        let files: Vec<&String> = files
            .iter()
            .filter(|f| !Self::is_internal_path(f))
            .collect();
        if files.is_empty() {
            return Ok("no files match".into());
        }
        let total = files.len();
        let mut s: String = files.iter().take(200).map(|f| format!("{f}\n")).collect();
        if total > 200 {
            s.push_str(&format!(
                "...[{} more files; narrow the glob]\n",
                total - 200
            ));
        }
        Ok(s)
    }

    async fn render_vera_results(&self, v: Value) -> Result<String, String> {
        let tracked = match self.head.as_deref() {
            Some(head) => Some(
                self.head_files
                    .get_or_try_init(|| async {
                        crate::git::tracked_files(&self.repo_root, head).await
                    })
                    .await
                    .map_err(|e| format!("list tracked files at reviewed head: {e}"))?,
            ),
            None => None,
        };
        let (v, omitted) = filter_search(v, tracked, |path| self.is_excluded(path));
        let mut output = render_search(&v);
        if omitted > 0 {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            if self.head.is_some() {
                output.push_str(&format!(
                    "[{omitted} result(s) outside the reviewed head or excluded by content policy omitted]\n"
                ));
            } else {
                output.push_str(&format!(
                    "[{omitted} result(s) excluded by content policy omitted]\n"
                ));
            }
        }
        Ok(output)
    }

    async fn call_inner(&self, name: &str, args: &Value) -> Result<String, String> {
        match name {
            "read_file" => self.read_file(args).await,
            "grep_repo" => self.grep_repo(args).await,
            "find_files" => self.find_files(args).await,
            "list_changed_files" => {
                let mut s = String::new();
                for f in &self.diff.files {
                    let status = match f.status {
                        crate::diff::FileStatus::Added => "A",
                        crate::diff::FileStatus::Modified => "M",
                        crate::diff::FileStatus::Deleted => "D",
                        crate::diff::FileStatus::Renamed => "R",
                    };
                    let (mut adds, mut dels) = (0u32, 0u32);
                    for h in &f.hunks {
                        for l in &h.lines {
                            match l.kind {
                                crate::diff::DiffLineKind::Add => adds += 1,
                                crate::diff::DiffLineKind::Del => dels += 1,
                                _ => {}
                            }
                        }
                    }
                    s.push_str(&format!("{} {} +{} -{}\n", f.new_path, status, adds, dels));
                }
                Ok(s)
            }
            "diff_context" => {
                let path = args["path"].as_str().ok_or("missing path")?;
                let line = args["line"].as_u64().ok_or("missing line")? as u32;
                self.diff
                    .hunk_containing(path, line)
                    .ok_or_else(|| format!("no hunk containing {path}:{line}"))
            }
            "vera_search" => {
                let q = args["query"].as_str().ok_or("missing query")?;
                let limit = args["limit"].as_u64().unwrap_or(8).min(20) as u32;
                let v = self
                    .vera
                    .search(
                        q,
                        args["intent"].as_str(),
                        args["path_glob"].as_str(),
                        args["lang"].as_str(),
                        limit,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                self.render_vera_results(v).await
            }
            "vera_references" => {
                let sym = args["symbol"].as_str().ok_or("missing symbol")?;
                let callees = args["direction"].as_str() == Some("callees");
                let limit = args["limit"].as_u64().unwrap_or(20) as u32;
                let v = self
                    .vera
                    .references(sym, callees, limit)
                    .await
                    .map_err(|e| e.to_string())?;
                self.render_vera_results(v).await
            }
            "vera_grep" => {
                let pat = args["pattern"].as_str().ok_or("missing pattern")?;
                let limit = args["limit"].as_u64().unwrap_or(30) as u32;
                let v = self
                    .vera
                    .grep(pat, args["path_glob"].as_str(), limit)
                    .await
                    .map_err(|e| e.to_string())?;
                self.render_vera_results(v).await
            }
            "vera_overview" => {
                let v = self.vera.overview().await.map_err(|e| e.to_string())?;
                Ok(serde_json::to_string(&v).unwrap_or_default())
            }
            other => Err(format!("unknown tool {other}")),
        }
    }

    /// Errors are returned to the model as a JSON tool result.
    pub async fn call(&self, name: &str, args: Value) -> String {
        let t0 = Instant::now();
        let (out, err) = self.call_checked(name, &args).await;
        {
            let mut st = self.stats.lock().unwrap();
            let e = st
                .by_tool
                .entry(name.to_string())
                .or_insert_with(|| ToolStat {
                    name: name.to_string(),
                    ..Default::default()
                });
            e.calls += 1;
            if err {
                e.errors += 1;
            }
            e.latency_ms += t0.elapsed().as_millis() as u64;
        }
        out
    }

    async fn call_checked(&self, name: &str, args: &Value) -> (String, bool) {
        if let Some(names) = &self.allowed
            && !names.iter().any(|n| n == name)
        {
            return (
                json!({"error": format!("tool {name} not available in this step")}).to_string(),
                true,
            );
        }
        if name.starts_with("vera_")
            && let Some(r) = self.vera_disabled.lock().unwrap().clone()
        {
            return (json!({"error": r}).to_string(), true);
        }
        match self.call_inner(name, args).await {
            Ok(s) => (self.truncate(crate::redact::text(&s).into_owned()), false),
            Err(e) => (json!({"error": crate::redact::text(&e)}).to_string(), true),
        }
    }
}

/// Render vera JSON results into compact `path:start-end [type name]` blocks.
fn render_search(v: &Value) -> String {
    let mut out = String::new();
    // Tolerate several shapes: {results:[...]}, [...], {matches:[...]}
    let items: Vec<&Value> = if let Some(a) = v.as_array() {
        a.iter().collect()
    } else if let Some(a) = v["results"].as_array() {
        a.iter().collect()
    } else if let Some(a) = v["matches"].as_array() {
        a.iter().collect()
    } else {
        return serde_json::to_string_pretty(v).unwrap_or_default();
    };
    for it in items {
        let path = it["path"]
            .as_str()
            .or_else(|| it["file_path"].as_str())
            .or_else(|| it["file"].as_str())
            .unwrap_or("?");
        let start = it["start_line"]
            .as_u64()
            .or_else(|| it["line_start"].as_u64())
            .or_else(|| it["line"].as_u64())
            .unwrap_or(0);
        let end = it["end_line"]
            .as_u64()
            .or_else(|| it["line_end"].as_u64())
            .unwrap_or(start);
        let st = it["symbol_type"]
            .as_str()
            .or_else(|| it["kind"].as_str())
            .unwrap_or("");
        let sn = it["symbol_name"]
            .as_str()
            .or_else(|| it["name"].as_str())
            .unwrap_or("");
        let content = it["content"]
            .as_str()
            .or_else(|| it["text"].as_str())
            .or_else(|| it["snippet"].as_str())
            .unwrap_or("");
        out.push_str(&format!("{path}:{start}-{end} [{st} {sn}]\n{content}\n\n"));
    }
    out
}

fn filter_search(
    mut v: Value,
    tracked: Option<&HashSet<String>>,
    is_excluded: impl Fn(&str) -> bool,
) -> (Value, usize) {
    let omitted = if v.is_array() {
        filter_search_items(
            v.as_array_mut().expect("array checked above"),
            tracked,
            &is_excluded,
        )
    } else if v["results"].is_array() {
        filter_search_items(
            v["results"].as_array_mut().expect("array checked above"),
            tracked,
            &is_excluded,
        )
    } else if v["matches"].is_array() {
        filter_search_items(
            v["matches"].as_array_mut().expect("array checked above"),
            tracked,
            &is_excluded,
        )
    } else {
        0
    };
    (v, omitted)
}

fn filter_search_items(
    items: &mut Vec<Value>,
    tracked: Option<&HashSet<String>>,
    is_excluded: &impl Fn(&str) -> bool,
) -> usize {
    let original_len = items.len();
    items.retain(|item| {
        let path = item["path"]
            .as_str()
            .or_else(|| item["file_path"].as_str())
            .or_else(|| item["file"].as_str());
        match path {
            Some(path) => !is_excluded(path) && tracked.is_none_or(|files| files.contains(path)),
            None => tracked.is_none(),
        }
    });
    original_len - items.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffSet;
    use crate::vera::VeraClient;
    use serde_json::json;
    use std::path::Path;

    fn run_git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn vera_tools_filter_working_tree_and_excluded_hits() {
        use std::os::unix::fs::PermissionsExt;

        let repo = tempfile::tempdir().unwrap();
        run_git(repo.path(), &["init", "-q"]);
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("src/lib.rs"), "tracked\n").unwrap();
        std::fs::write(repo.path().join(".env"), "secret\n").unwrap();
        run_git(repo.path(), &["add", "--", "src/lib.rs", ".env"]);
        run_git(
            repo.path(),
            &[
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "base",
            ],
        );
        let head = crate::git::resolve_commit(repo.path(), "HEAD")
            .await
            .unwrap();
        std::fs::write(repo.path().join("working-only.rs"), "untracked\n").unwrap();
        let exe = repo.path().join("fake-vera");
        std::fs::write(
            &exe,
            r#"#!/bin/sh
cat <<'JSON'
{"results":[
  {"path":"src/lib.rs","start_line":1,"content":"tracked result"},
  {"file_path":"working-only.rs","start_line":1,"content":"untracked result"},
  {"file":".env","start_line":1,"content":"excluded result"},
  {"content":"pathless result"}
]}
JSON
"#,
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let vera = VeraClient {
            exe,
            repo_root: repo.path().to_path_buf(),
            env: vec![],
            backend: "local".into(),
            exclude: vec![".env".into()],
            home: repo.path().join(".vera"),
            rerank: None,
            embedding_pairs: vec![],
            index_key: String::new(),
            rerank_fallbacks: Default::default(),
            deadline: None,
        };
        let toolbox = ToolBox::new(
            repo.path().to_path_buf(),
            Arc::new(DiffSet::default()),
            Arc::new(vera),
            10_000,
        )
        .with_head(&head);

        for (name, args) in [
            ("vera_search", json!({"query":"test"})),
            ("vera_references", json!({"symbol":"test"})),
            ("vera_grep", json!({"pattern":"test"})),
        ] {
            let output = toolbox.call(name, args).await;
            assert!(output.contains("src/lib.rs:1-1"), "{output}");
            assert!(output.contains("tracked result"), "{output}");
            assert!(
                output.contains(
                    "[3 result(s) outside the reviewed head or excluded by content policy omitted]"
                ),
                "{output}"
            );
            assert!(!output.contains("working-only.rs"), "{output}");
            assert!(!output.contains(".env"), "{output}");
            assert!(!output.contains("pathless result"), "{output}");
            assert!(!output.contains("excluded result"), "{output}");
        }

        let invalid_head = ToolBox::new(
            repo.path().to_path_buf(),
            Arc::new(DiffSet::default()),
            toolbox.vera.clone(),
            10_000,
        )
        .with_head("not-a-resolved-commit");
        let output = invalid_head
            .call("vera_search", json!({"query":"test"}))
            .await;
        assert!(
            output.contains("list tracked files at reviewed head"),
            "{output}"
        );
        assert!(!output.contains("tracked result"), "{output}");

        let no_head = ToolBox::new(
            repo.path().to_path_buf(),
            Arc::new(DiffSet::default()),
            toolbox.vera.clone(),
            10_000,
        );
        let output = no_head.call("vera_search", json!({"query":"test"})).await;
        assert!(output.contains("src/lib.rs:1-1"), "{output}");
        assert!(output.contains("working-only.rs:1-1"), "{output}");
        assert!(output.contains("pathless result"), "{output}");
        assert!(
            output.contains("[1 result(s) excluded by content policy omitted]"),
            "{output}"
        );
        assert!(!output.contains(".env"), "{output}");
        assert!(!output.contains("excluded result"), "{output}");
    }

    #[test]
    fn vera_working_directories_are_internal() {
        for p in [
            ".vera/index.db",
            ".vera.build/x",
            ".vera.resume/embeddings.db",
            "sub/.revera/state.json",
            ".git/config",
        ] {
            assert!(ToolBox::is_internal_path(p), "{p}");
        }
        for p in ["src/vera.rs", ".veral/x", "docs/.vera.md"] {
            assert!(!ToolBox::is_internal_path(p), "{p}");
        }
    }

    #[test]
    fn filter_search_handles_array_and_matches_shapes() {
        let files = HashSet::from(["tracked.rs".to_string()]);
        for value in [
            json!([{"path":"tracked.rs"},{"file":"loose.rs"}]),
            json!({"matches":[{"file_path":"tracked.rs"},{"file":"loose.rs"}]}),
        ] {
            let (filtered, omitted) = filter_search(value, Some(&files), |path| path == ".env");
            assert_eq!(omitted, 1);
            assert_eq!(
                filtered
                    .as_array()
                    .or_else(|| filtered["matches"].as_array())
                    .unwrap()
                    .len(),
                1
            );
        }
    }
}
