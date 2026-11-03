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
// Mirrors ProfileOverride::builtin, including the saturating request multiplier.
const triple = (value: number) => Math.min(0xffffffff, value * 3);
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
    agent_max_tool_calls: b.agent_max_tool_calls,
    agent_max_seconds: b.agent_max_seconds,
    run_max_seconds: Math.min(b.run_max_seconds * 2, 3000),
    run_max_requests: triple(b.run_max_requests),
    review: {
      ...review,
      max_diff_bytes: Math.max(review.max_diff_bytes, 400000),
      max_findings: Math.max(review.max_findings, 20),
    },
  };
}
