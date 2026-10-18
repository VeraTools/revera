export type ThemeChoice = 'system' | 'light' | 'dark';

const stored = typeof document === 'undefined' ? undefined : document.documentElement.dataset.theme;
export const theme = $state<{ choice: ThemeChoice }>({
  choice: stored === 'light' || stored === 'dark' ? stored : 'system',
});

export function setTheme(choice: ThemeChoice) {
  theme.choice = choice;
  if (typeof document === 'undefined') return;
  if (choice === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.dataset.theme = choice;
  try {
    if (choice === 'system') localStorage.removeItem('revera-theme');
    else localStorage.setItem('revera-theme', choice);
  } catch {}
}
