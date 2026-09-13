use super::api::{GitHubApi, ReviewComment};
use super::event::PrEvent;
use crate::report::{Publication, RunReport};
use crate::state::{FindingState, ReviewState};
use anyhow::Result;
use base64::Engine;

const STATE_PREFIX: &str = "<!-- revera-state:";
const STATE_SUFFIX: &str = " -->";

/// `<!-- revera-state:<base64(json)> -->`
pub fn encode_state(s: &ReviewState) -> String {
    let json = serde_json::to_string(s).unwrap_or_default();
    format!(
        "{}{}{}",
        STATE_PREFIX,
        base64::engine::general_purpose::STANDARD.encode(json),
        STATE_SUFFIX
    )
}

/// Extract the state blob from a managed summary comment body.
pub fn decode_state(body: &str) -> Option<ReviewState> {
    let start = body.find(STATE_PREFIX)? + STATE_PREFIX.len();
    let end = body[start..].find(STATE_SUFFIX)? + start;
    let b64 = body[start..end].trim();
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn revera_id(body: &str) -> Option<String> {
    let marker = "<!-- revera-id:";
    let start = body.find(marker)? + marker.len();
    let end = body[start..].find("-->")? + start;
    Some(body[start..end].trim().to_string())
}

/// Run the comment-mode publisher against the GitHub API.
///
/// Steps: (1) re-check head sha, (2) post review with unposted inline
/// comments, (3) upsert the managed summary comment carrying the state blob,
/// (4) mark posted ids in `state`.
///
/// `resolved_titles`: titles of findings resolved by rechecks this run.
pub async fn publish(
    api: &GitHubApi,
    ev: &PrEvent,
    report: &mut RunReport,
    state: &mut ReviewState,
    max_findings: usize,
    summary_marker: &str,
    resolved_titles: &[String],
) -> Result<Publication> {
    let (owner, repo) = ev.owner_repo();
    let mut pubn = Publication {
        mode: "comment".into(),
        ..Default::default()
    };

    // (1) head-moved check: fail closed
    let live_head = api.get_pull(owner, repo, ev.number).await?;
    if live_head != report.head {
        let reason = format!("head moved {} -> {}", report.head, live_head);
        pubn.skipped_reason = Some(reason.clone());
        report.status = crate::report::RunStatus::Partial;
        report.reason = Some(match report.reason.take() {
            Some(r) => format!("{r}; {reason}"),
            None => reason,
        });
        report.publication = pubn;
        return Ok(report.publication.clone());
    }

    // (2) review with inline comments for accepted+Inline, not yet posted
    let mut posted_ids: Vec<String> = Vec::new();
    let comments: Vec<ReviewComment> = report
        .plan
        .inline
        .iter()
        .filter(|c| {
            revera_id(&c.body)
                .map(|id| !state.has_posted(&id))
                .unwrap_or(false)
        })
        .take(max_findings)
        .map(|c| ReviewComment {
            path: c.file.clone(),
            line: c.line,
            end_line: c.end_line,
            body: c.body.clone(),
        })
        .collect();
    if !comments.is_empty() {
        for c in &comments {
            if let Some(id) = revera_id(&c.body) {
                posted_ids.push(id);
            }
        }
        let review_id = api
            .create_review(
                owner,
                repo,
                ev.number,
                &report.head,
                "Revera inline review findings",
                &comments,
            )
            .await?;
        pubn.review_id = Some(review_id);
        // (4) mark posted ids only after a successful post, so the state blob
        // embedded below carries them
        state.mark_posted(&posted_ids);
    }

    // (3) upsert the managed summary comment
    let mut body = format!("{summary_marker}\n{}", report.plan.summary_markdown);
    // all currently open findings (incl. previously posted)
    let open: Vec<_> = state
        .findings
        .iter()
        .filter(|f| f.status == FindingState::Open)
        .collect();
    if !open.is_empty() {
        body.push_str("\n### Open findings\n\n");
        for f in &open {
            body.push_str(&format!(
                "- `{}`:{} — {}{}\n",
                f.file,
                f.start_line,
                f.title,
                if f.posted { " (posted)" } else { "" }
            ));
        }
    }
    if !resolved_titles.is_empty() {
        body.push_str("\nResolved since last review:\n");
        for t in resolved_titles {
            body.push_str(&format!("- {t}\n"));
        }
    }
    body.push('\n');
    body.push_str(&encode_state(state));

    let comments = api.list_issue_comments(owner, repo, ev.number).await?;
    let managed = comments
        .iter()
        .find(|c| c.body.contains(summary_marker))
        .map(|c| c.id);
    let comment = match managed {
        Some(id) => api.update_issue_comment(owner, repo, id, &body).await?,
        None => {
            api.create_issue_comment(owner, repo, ev.number, &body)
                .await?
        }
    };
    pubn.summary_comment_id = Some(comment.id);

    report.publication = pubn.clone();
    Ok(pubn)
}
