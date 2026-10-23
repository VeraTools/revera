const labels: Record<string, string> = {
  pull_request: 'Pull request',
  webhook: 'Push',
  reconcile: 'Background check',
};

export function triggerLabel(trigger: string): string {
  const label = labels[trigger] ?? trigger;
  return label.charAt(0).toUpperCase() + label.slice(1);
}
