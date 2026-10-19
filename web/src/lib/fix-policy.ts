import type { FixPolicy } from './types';

// Keep aligned with server fixes::Policy::validate.
export const fixLimitRanges = {
  attempts: [1, 10],
  tool_calls: [1, 1000],
  seconds: [1, 3600],
  model_requests: [2, 1000],
  max_files: [1, 100],
  max_patch_kb: [1, 512],
  concurrent: [1, 16],
  per_user_hour: [1, 1000],
  per_repo_day: [1, 10000],
} as const;

export function validFixLimits(policy: FixPolicy): boolean {
  return (
    Number.isInteger(policy.test_timeout_seconds) &&
    policy.test_timeout_seconds >= 1 &&
    policy.test_timeout_seconds <= 3600 &&
    (Object.keys(fixLimitRanges) as (keyof typeof fixLimitRanges)[]).every((key) => {
      const value = policy.limits[key];
      const [min, max] = fixLimitRanges[key];
      return Number.isInteger(value) && value >= min && value <= max;
    })
  );
}
