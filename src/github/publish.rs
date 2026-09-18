use super::api::{GitHubApi, GitHubHttpError, ReviewComment};
use super::event::PrEvent;
use crate::report::{surfaced_ids, Publication, RunReport};
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
        report
            .timing
            .append_publish(report.timing.total_ms, 0, "skipped");
        report.plan.summary_markdown =
            crate::report::refresh_timing_line(&report.plan.summary_markdown, &report.timing);
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
    // the publish phase measures the inline review publication only; the
    // summary upsert below is excluded by design (it cannot time itself)
    let review_start = std::time::Instant::now();
    let mut review_outcome = "skipped";
    if !comments.is_empty() {
        for c in &comments {
            if let Some(id) = revera_id(&c.body) {
                posted_ids.push(id);
            }
        }
        let review_result = api
            .create_review(
                owner,
                repo,
                ev.number,
                &report.head,
                "Revera inline review findings",
                &comments,
            )
            .await;
        match review_result {
            Ok(review_id) => {
                pubn.review_id = Some(review_id);
                review_outcome = "ok";
                // (4) mark posted ids only after a successful post, so the state blob
                // embedded below carries them
                state.mark_posted(&posted_ids);
            }
            Err(err)
                if err
                    .downcast_ref::<GitHubHttpError>()
                    .is_some_and(|e| e.status == 422) =>
            {
                tracing::warn!("GitHub rejected inline review (422); continuing with summary");
                pubn.skipped_reason = Some(
                    "inline review rejected by GitHub (422); findings listed in summary only"
                        .into(),
                );
                review_outcome = "ok:inline-rejected";
            }
            Err(err) => {
                report.timing.append_publish(
                    report.timing.total_ms,
                    review_start.elapsed().as_millis() as u64,
                    &format!("error:{}", crate::text::excerpt_bytes(&err.to_string(), 60)),
                );
                return Err(err);
            }
        }
    }
    report.timing.append_publish(
        report.timing.total_ms,
        if review_outcome == "skipped" {
            0
        } else {
            review_start.elapsed().as_millis() as u64
        },
        review_outcome,
    );
    // refresh the timing line so the summary comment and the JSON report
    // tell the same story
    report.plan.summary_markdown =
        crate::report::refresh_timing_line(&report.plan.summary_markdown, &report.timing);

    // (3) upsert the managed summary comment
    let mut staged = state.clone();
    staged.mark_posted(&surfaced_ids(report));
    let mut body = format!("{summary_marker}\n{}", report.plan.summary_markdown);
    // all currently open findings (incl. previously posted)
    let open: Vec<_> = staged
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
    body.push_str(&encode_state(&staged));

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
    *state = staged;

    report.publication = pubn.clone();
    Ok(pubn)
}
