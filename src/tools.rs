use crate::diff::DiffSet;
use crate::provider::ToolSpec;
use crate::vera::VeraClient;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct ToolBox {
    pub repo_root: PathBuf,
    pub diff: Arc<DiffSet>,
    pub vera: Arc<VeraClient>,
    pub max_output_bytes: usize,
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
                "findings": {"type": "array", "items": {"type": "object"}},
                "coverage": {"type": "string"},
            }),
            &["findings", "coverage"],
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
        Self {
            repo_root,
            diff,
            vera,
            max_output_bytes,
        }
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        vec![
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
        ]
    }

    fn truncate(&self, s: String) -> String {
        if s.len() <= self.max_output_bytes {
            s
        } else {
            let total = s.len();
            let mut cut = s[..self.max_output_bytes].to_string();
            cut.push_str(&format!("...[truncated, {total} bytes total]"));
            cut
        }
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
            if s == ".git" || s == ".vera" || s == ".revera" {
                return Err(format!("path under {s} is not readable"));
            }
        }
        Ok(canon)
    }

    async fn read_file(&self, args: &Value) -> Result<String, String> {
        let path = args["path"].as_str().ok_or("missing path")?;
        let canon = self.resolve_path(path)?;
        let text = std::fs::read_to_string(&canon).map_err(|e| format!("read {path}: {e}"))?;
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
            if let Some(e) = end {
                if ln > e {
                    break;
                }
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

    async fn call_inner(&self, name: &str, args: &Value) -> Result<String, String> {
        match name {
            "read_file" => self.read_file(args).await,
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
                Ok(render_search(&v))
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
                Ok(render_search(&v))
            }
            "vera_grep" => {
                let pat = args["pattern"].as_str().ok_or("missing pattern")?;
                let limit = args["limit"].as_u64().unwrap_or(30) as u32;
                let v = self
                    .vera
                    .grep(pat, args["path_glob"].as_str(), limit)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(render_search(&v))
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
        let res = self.call_inner(name, &args).await;
        let s = match res {
            Ok(s) => s,
            Err(e) => json!({"error": e}).to_string(),
        };
        self.truncate(s)
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
