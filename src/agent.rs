use crate::provider::{ChatMessage, ModelClient, ProviderError, ToolCall, ToolSpec};
use crate::tools::ToolBox;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Terminal,
    ToolBudget,
    TimeBudget,
    NoTerminalCall,
    ProviderError,
}

#[derive(Debug)]
pub struct AgentRun {
    pub final_call: Option<ToolCall>,
    pub transcript_len: usize,
    pub tool_calls: u32,
    pub stopped: StopReason,
    /// The terminal call failed `check` and one bounded repair round was
    /// attempted (its result, valid or not, is `final_call`).
    pub repaired: bool,
}

/// Validates a terminal call's arguments; `Err(msg)` is fed back to the
/// model once as a tool result so it can resubmit.
pub type TerminalCheck<'a> = &'a (dyn Fn(&serde_json::Value) -> Result<(), String> + Sync);

#[derive(Debug, Clone)]
pub struct AgentBudget {
    pub max_tool_calls: u32,
    pub max_seconds: u64,
}

async fn run_tool_calls(toolbox: &ToolBox, calls: &[ToolCall]) -> Vec<(String, String, String)> {
    futures::future::join_all(calls.iter().map(|c| {
        let name = c.name.clone();
        let args = c.arguments.clone();
        let id = c.id.clone();
        async move {
            let res = toolbox.call(&name, args).await;
            (id, name, res)
        }
    }))
    .await
}

/// Try to interpret a bare-text assistant reply as the terminal tool's args.
/// The object must carry at least one of the terminal tool's required
/// properties, so an unrelated `{}` or prose-with-JSON is not mistaken for
/// a submission.
fn text_as_terminal(content: &str, terminal_tool: &ToolSpec) -> Option<ToolCall> {
    let t = content.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(t);
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    let required: Vec<&str> = terminal_tool.parameters["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r.as_str()).collect())
        .unwrap_or_default();
    let has_required = match v.as_object() {
        Some(o) => required.is_empty() || required.iter().any(|r| o.contains_key(*r)),
        None => false,
    };
    if has_required {
        return Some(ToolCall {
            id: "text-terminal".into(),
            name: String::new(),
            arguments: v,
        });
    }
    None
}

pub async fn run_agent(
    client: &dyn ModelClient,
    system: &str,
    user: &str,
    toolbox: &ToolBox,
    terminal_tool: &ToolSpec,
    budget: &AgentBudget,
) -> Result<AgentRun, ProviderError> {
    run_agent_checked(
        client,
        system,
        user,
        toolbox,
        terminal_tool,
        budget,
        &|_| Ok(()),
    )
    .await
}

/// Minimum time that must remain for a repair round to be attempted.
const REPAIR_MIN_LEFT: Duration = Duration::from_secs(15);

/// Allowance for the single completion that answers the "budget exhausted"
/// notice once the time budget is already spent.
const TERMINAL_GRACE: Duration = Duration::from_secs(20);

/// Like `run_agent`, but the terminal call's arguments are validated with
/// `check`. On failure (and while time remains) the error is returned to
/// the model once and it may resubmit; the second submission is final.
/// Every provider request and tool batch is bounded by the remaining
/// time budget, so a hung provider or tool cannot outlive the deadline.
pub async fn run_agent_checked(
    client: &dyn ModelClient,
    system: &str,
    user: &str,
    toolbox: &ToolBox,
    terminal_tool: &ToolSpec,
    budget: &AgentBudget,
    check: TerminalCheck<'_>,
) -> Result<AgentRun, ProviderError> {
    let mut messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let mut specs = toolbox.specs();
    specs.push(terminal_tool.clone());
    let mut tool_calls = 0u32;
    let mut nudged = false;
    let mut repaired = false;
    let mut budget_notice_sent = false;
    let mut budget_stop = StopReason::ToolBudget;
    let start = Instant::now();
    let total = Duration::from_secs(budget.max_seconds);
    // hard cap on any single await: the remaining budget; only the reply to
    // a time-budget notice gets a bounded grace allowance
    let left = |grace: bool| {
        let rem = total.saturating_sub(start.elapsed());
        if grace {
            rem.max(TERMINAL_GRACE)
        } else {
            rem.max(Duration::from_secs(1))
        }
    };

    loop {
        let time_up = start.elapsed() > total;
        let tools_up = tool_calls >= budget.max_tool_calls;
        if (time_up || tools_up) && !budget_notice_sent {
            budget_notice_sent = true;
            budget_stop = if time_up {
                StopReason::TimeBudget
            } else {
                StopReason::ToolBudget
            };
            messages.push(ChatMessage::user(format!(
                "Budget exhausted; call {} now with what you have",
                terminal_tool.name
            )));
        }

        let grace = budget_notice_sent && budget_stop == StopReason::TimeBudget;
        let completion =
            match tokio::time::timeout(left(grace), client.complete(&messages, &specs)).await {
                Ok(Ok(c)) => c,
                Ok(Err(e)) => {
                    tracing::warn!("model request failed: {e}");
                    return Ok(AgentRun {
                        final_call: None,
                        transcript_len: messages.len(),
                        tool_calls,
                        stopped: StopReason::ProviderError,
                        repaired,
                    });
                }
                Err(_) => {
                    tracing::warn!("model request exceeded the remaining time budget");
                    return Ok(AgentRun {
                        final_call: None,
                        transcript_len: messages.len(),
                        tool_calls,
                        stopped: StopReason::TimeBudget,
                        repaired,
                    });
                }
            };
        let msg = completion.message;
        messages.push(msg.clone());

        // a terminal submission (tool call or bare JSON text) is sent back
        // once for repair when `check` rejects it and time remains
        let repair_msg = |tc: &ToolCall, repaired: bool| -> Option<String> {
            let err = check(&tc.arguments).err()?;
            if repaired || total.saturating_sub(start.elapsed()) < REPAIR_MIN_LEFT {
                return None;
            }
            tracing::warn!("terminal call rejected, requesting one repair: {err}");
            Some(format!(
                "{err}. Call `{}` again with a corrected submission; this is the last attempt.",
                terminal_tool.name
            ))
        };

        if msg.tool_calls.is_empty() {
            if let Some(content) = &msg.content {
                if let Some(mut tc) = text_as_terminal(content, terminal_tool) {
                    tc.name = terminal_tool.name.clone();
                    // a text reply has no tool_call id to answer; the repair
                    // request becomes a user turn
                    if let Some(m) = repair_msg(&tc, repaired) {
                        repaired = true;
                        messages.push(ChatMessage::user(m));
                        continue;
                    }
                    return Ok(AgentRun {
                        final_call: Some(tc),
                        transcript_len: messages.len(),
                        tool_calls,
                        stopped: StopReason::Terminal,
                        repaired,
                    });
                }
                if !nudged && !budget_notice_sent {
                    nudged = true;
                    messages.push(ChatMessage::user(format!(
                        "Call `{}` to finish.",
                        terminal_tool.name
                    )));
                    continue;
                }
            }
            return Ok(AgentRun {
                final_call: None,
                transcript_len: messages.len(),
                tool_calls,
                stopped: if budget_notice_sent {
                    budget_stop
                } else {
                    StopReason::NoTerminalCall
                },
                repaired,
            });
        }

        // check terminal first
        if let Some(c) = msg.tool_calls.iter().find(|c| c.name == terminal_tool.name) {
            let Some(m) = repair_msg(c, repaired) else {
                return Ok(AgentRun {
                    final_call: Some(c.clone()),
                    transcript_len: messages.len(),
                    tool_calls,
                    stopped: StopReason::Terminal,
                    repaired,
                });
            };
            repaired = true;
            messages.push(ChatMessage::tool(
                c.id.clone(),
                c.name.clone(),
                serde_json::json!({"error": m}).to_string(),
            ));
            // answer the sibling tool calls so the transcript stays valid,
            // then let the model resubmit
            for other in msg.tool_calls.iter().filter(|o| o.id != c.id) {
                messages.push(ChatMessage::tool(
                    other.id.clone(),
                    other.name.clone(),
                    r#"{"error":"not executed: resubmit the terminal call"}"#.to_string(),
                ));
            }
            continue;
        }

        // After the budget notice, a non-terminal completion ends the loop
        // immediately — no further tool calls are executed.
        if budget_notice_sent {
            return Ok(AgentRun {
                final_call: None,
                transcript_len: messages.len(),
                tool_calls,
                stopped: budget_stop,
                repaired,
            });
        }

        // cap the batch at the remaining tool budget; skipped calls still get
        // tool results so the transcript stays valid for the API
        let remaining = budget.max_tool_calls.saturating_sub(tool_calls) as usize;
        let (exec, skipped) = msg.tool_calls.split_at(remaining.min(msg.tool_calls.len()));
        let results = match tokio::time::timeout(left(false), run_tool_calls(toolbox, exec)).await {
            Ok(r) => r,
            Err(_) => {
                tracing::warn!("tool batch exceeded the remaining time budget");
                return Ok(AgentRun {
                    final_call: None,
                    transcript_len: messages.len(),
                    tool_calls,
                    stopped: StopReason::TimeBudget,
                    repaired,
                });
            }
        };
        tool_calls += exec.len() as u32;
        for (id, name, res) in results {
            messages.push(ChatMessage::tool(id, name, res));
        }
        for c in skipped {
            messages.push(ChatMessage::tool(
                c.id.clone(),
                c.name.clone(),
                r#"{"error":"not executed: budget"}"#.to_string(),
            ));
        }
    }
}
