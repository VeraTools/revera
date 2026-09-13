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
}

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
fn text_as_terminal(content: &str) -> Option<ToolCall> {
    let t = content.trim();
    let t = t
        .strip_prefix("```json")
        .or_else(|| t.strip_prefix("```"))
        .and_then(|s| s.strip_suffix("```"))
        .map(|s| s.trim())
        .unwrap_or(t);
    let v: serde_json::Value = serde_json::from_str(t).ok()?;
    if v.is_object() {
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
    let mut messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
    let mut specs = toolbox.specs();
    specs.push(terminal_tool.clone());
    let mut tool_calls = 0u32;
    let mut nudged = false;
    let mut budget_notice_sent = false;
    let mut budget_stop = StopReason::ToolBudget;
    let start = Instant::now();

    loop {
        let time_up = start.elapsed() > Duration::from_secs(budget.max_seconds);
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

        let completion = match client.complete(&messages, &specs).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("model request failed: {e}");
                return Ok(AgentRun {
                    final_call: None,
                    transcript_len: messages.len(),
                    tool_calls,
                    stopped: StopReason::ProviderError,
                });
            }
        };
        let msg = completion.message;
        messages.push(msg.clone());

        if msg.tool_calls.is_empty() {
            if let Some(content) = &msg.content {
                if let Some(mut tc) = text_as_terminal(content) {
                    tc.name = terminal_tool.name.clone();
                    return Ok(AgentRun {
                        final_call: Some(tc),
                        transcript_len: messages.len(),
                        tool_calls,
                        stopped: StopReason::Terminal,
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
            });
        }

        // check terminal first
        for c in &msg.tool_calls {
            if c.name == terminal_tool.name {
                return Ok(AgentRun {
                    final_call: Some(c.clone()),
                    transcript_len: messages.len(),
                    tool_calls,
                    stopped: StopReason::Terminal,
                });
            }
        }

        // After the budget notice, a non-terminal completion ends the loop
        // immediately — no further tool calls are executed.
        if budget_notice_sent {
            return Ok(AgentRun {
                final_call: None,
                transcript_len: messages.len(),
                tool_calls,
                stopped: budget_stop,
            });
        }

        // cap the batch at the remaining tool budget; skipped calls still get
        // tool results so the transcript stays valid for the API
        let remaining = budget.max_tool_calls.saturating_sub(tool_calls) as usize;
        let (exec, skipped) = msg.tool_calls.split_at(remaining.min(msg.tool_calls.len()));
        let results = run_tool_calls(toolbox, exec).await;
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
