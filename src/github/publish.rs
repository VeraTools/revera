use super::api::{GhComment, GitHubApi, GitHubHttpError, ReviewComment};
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

/// Pick the reviewer-owned managed summary comment.
///
/// A candidate must *start* with the marker and carry a decodable state
/// blob — a quoted or copied marker inside someone else's comment does
/// not qualify. Among candidates: the comment id recorded in prior state
/// wins; otherwise the author must be the authenticated identity when it
/// is known, or a bot account (the Actions token cannot name itself and
/// posts as `github-actions[bot]`). Comments with no author information
/// (older API shapes, tests) are accepted.
pub fn find_managed<'a>(
    comments: &'a [GhComment],
    marker: &str,
    expected_id: Option<u64>,
    viewer: Option<&str>,
) -> Option<&'a GhComment> {
    let mut cands = comments
        .iter()
        .filter(|c| c.body.trim_start().starts_with(marker) && decode_state(&c.body).is_some());
    if let Some(id) = expected_id {
        if let Some(c) = comments
            .iter()
            .find(|c| c.id == id && c.body.trim_start().starts_with(marker))
        {
            return Some(c);
        }
    }
    cands.find(|c| match (&c.author, viewer) {
        (None, _) => true,
        (Some(a), Some(v)) => a == v,
        (Some(a), None) => c.author_is_bot || a.ends_with("[bot]"),
    })
}

/// Revera ids already present as inline review comments on the PR: the
/// durable record of what was posted, independent of the summary blob.
pub fn posted_revera_ids(review_comment_bodies: &[String]) -> Vec<String> {
    review_comment_bodies
        .iter()
        .filter_map(|b| revera_id(b))
        .collect()
}

fn revera_id(body: &str) -> Option<String> {
    let marker = "<!-- revera-id:";
    let start = body.find(marker)? + marker.len();
    let end = body[start..].find("-->")? + start;
    Some(body[start..end].trim().to_string())
}

/// All currently open findings (incl. previously posted).
fn open_section(state: &ReviewState) -> String {
    let open: Vec<_> = state
        .findings
        .iter()
        .filter(|f| f.status == FindingState::Open)
        .collect();
    let mut s = String::new();
    if !open.is_empty() {
        s.push_str("\n### Open findings\n\n");
        for f in &open {
            s.push_str(&format!(
                "- `{}`:{} — {}{}\n",
                f.file,
                f.start_line,
                f.title,
                if f.posted { " (posted)" } else { "" }
            ));
        }
    }
    s
}

/// Run the comment-mode publisher against the GitHub API.
///
/// Steps: (1) re-check head sha, (2) post review with unposted inline
/// comments, (3) upsert the managed summary comment carrying the state blob,
/// (4) mark posted ids in `state`.
pub async fn publish(
    api: &GitHubApi,
    ev: &PrEvent,
    report: &mut RunReport,
    state: &mut ReviewState,
    max_findings: usize,
    summary_marker: &str,
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

    // (2) review with inline comments for accepted+Inline, not yet posted.
    // Inline comments already on the PR count as posted even when a prior
    // summary upsert failed before it could record them.
    let already = match api.list_review_comment_bodies(owner, repo, ev.number).await {
        Ok(bodies) => posted_revera_ids(&bodies),
        Err(e) => {
            tracing::warn!("could not list existing review comments: {e:#}");
            vec![]
        }
    };
    if !already.is_empty() {
        state.mark_posted(&already);
    }
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
    body.push_str(&open_section(&staged));
    let comments = api.list_issue_comments(owner, repo, ev.number).await?;
    let viewer = api.viewer_login().await;
    let managed = find_managed(
        &comments,
        summary_marker,
        state.summary_comment_id,
        viewer.as_deref(),
    )
    .map(|c| c.id);
    // the state blob records which comment is ours so the next run can
    // find it even if someone copies the marker
    let comment = match managed {
        Some(id) => {
            staged.summary_comment_id = Some(id);
            body.push('\n');
            body.push_str(&encode_state(&staged));
            api.update_issue_comment(owner, repo, id, &body).await?
        }
        None => {
            body.push('\n');
            body.push_str(&encode_state(&staged));
            let created = api
                .create_issue_comment(owner, repo, ev.number, &body)
                .await?;
            if created.id != 0 {
                staged.summary_comment_id = Some(created.id);
                let mut b2 = format!("{summary_marker}\n{}", report.plan.summary_markdown);
                b2.push_str(&open_section(&staged));
                b2.push('\n');
                b2.push_str(&encode_state(&staged));
                // best effort: the id is a convenience, ownership is also
                // checked by author
                if let Err(e) = api.update_issue_comment(owner, repo, created.id, &b2).await {
                    tracing::warn!("could not record summary comment id: {e:#}");
                }
            }
            created
        }
    };
    pubn.summary_comment_id = Some(comment.id);
    *state = staged;

    report.publication = pubn.clone();
    Ok(pubn)
}
