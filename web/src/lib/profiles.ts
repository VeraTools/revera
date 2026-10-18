export interface Budget {
  agent_max_tool_calls: number;
  agent_max_seconds: number;
  run_max_seconds: number;
  run_max_requests: number;
  retries: number;
}
interface ReviewBudget {
  max_diff_bytes: number;
  max_findings: number;
}
// Mirrors the server's saturating u32 arithmetic (seconds are capped at 3000 anyway).
const twice = (value: number) => Math.min(0xffffffff, value * 2);
export function quickBudget(b: Budget): Budget {
  return {
    ...b,
    agent_max_tool_calls: Math.max(1, Math.floor(b.agent_max_tool_calls / 2)),
    agent_max_seconds: Math.max(30, Math.floor(b.agent_max_seconds / 2)),
    run_max_seconds: Math.max(60, Math.floor(b.run_max_seconds / 2)),
  };
}
export function deepBudget(b: Budget, review: ReviewBudget) {
  return {
    ...b,
    agent_max_tool_calls: twice(b.agent_max_tool_calls),
    agent_max_seconds: Math.max(b.agent_max_seconds, Math.min(twice(b.agent_max_seconds), 3000)),
    run_max_seconds: Math.max(b.run_max_seconds, Math.min(twice(b.run_max_seconds), 3000)),
    run_max_requests: twice(b.run_max_requests),
    review: {
      ...review,
      max_diff_bytes: Math.max(review.max_diff_bytes, 400000),
      max_findings: Math.max(review.max_findings, 20),
    },
  };
}
