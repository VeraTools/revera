export function triggerLabel(trigger: string): string {
  return trigger === 'pull_request' ? 'Pull request' : trigger;
}
