import { guidanceDefaults, isGuidanceMode, maxNotesCharacters, notesCharacters } from './guidance';
import type { GuidancePreview, RepoGuidanceSettings, Repository } from './types';

// Guidance fixtures are independent of pull-request and fix inventory fixtures.
export const mockGuidanceNotes =
  'Focus on request authorization, cancellation and recovery. Ignore cosmetic naming changes.';
const skillDescriptions: [string, string][] = [
  [
    'api-contracts',
    'Review API request validation, response shapes and compatibility. Trace callers before changing a public contract.',
  ],
  [
    'authentication',
    'Check session expiry and authentication boundaries. Verify that rejected credentials never reach protected handlers.',
  ],
  [
    'background-jobs',
    'Follow job admission, cancellation and recovery. Confirm that retries cannot duplicate externally visible work.',
  ],
  [
    'database-migrations',
    'Inspect schema migrations and compatibility during rolling deployments. Check that existing records keep their meaning.',
  ],
  [
    'error-handling',
    'Trace errors from their origin to the user. Keep failures actionable without leaking credentials or private content.',
  ],
  [
    'frontend-accessibility',
    'Review keyboard navigation, labels and focus restoration. Test loading and empty states on small screens.',
  ],
  [
    'observability',
    'Check structured logs and operational signals. Distinguish a clean result from work that was not completed.',
  ],
  [
    'rust-concurrency',
    'Inspect shared state and lock ordering in asynchronous Rust. Check cancellation paths and avoid holding locks over network calls.',
  ],
  [
    'security-review',
    'Follow untrusted inputs through authorization and persistence. Confirm that repository content cannot change operator policy.',
  ],
  [
    'testing',
    'Add a focused regression test for the failing path. Keep assertions meaningful and use offline fixtures for external services.',
  ],
];
const contents: Record<string, string> = {
  'AGENTS.md':
    '# Project conventions\n\nKeep authentication checks before any credential access.\nRead repository content from immutable commits.\n\n## Review checklist\n\n- Trace cancellation and restart recovery.\n- Add an offline regression test for each concrete defect.\n',
  'CLAUDE.md':
    '# Coding agent notes\n\nAGENTS.md is the primary source of project conventions. Keep changes small and preserve public API compatibility.\n',
  'src/server/AGENTS.md':
    '# Server invariants\n\nNever hold the dashboard lock over a network request. Cancelled jobs must not be recorded as completed.\n',
  'docs/REVIEW.md':
    '# Documentation reviews\n\nCheck that documented commands and configuration defaults match the implementation. Do not report stylistic preferences as defects.\n',
  ...Object.fromEntries(
    skillDescriptions.map(([name, description]) => [
      `.agents/skills/${name}/SKILL.md`,
      `---\nname: ${name}\ndescription: >-\n  ${description}\n---\n\n# ${name.split('-').join(' ')}\n\n${description}\n\n1. Read the changed code and its callers.\n2. Check the project invariants and relevant tests.\n3. Report only concrete, reachable defects introduced by the change.\n`,
    ]),
  ),
};
const bytes = (text: string) => new TextEncoder().encode(text).length;
export function mockGuidanceRequest(
  url: URL,
  method: string,
  repos: Repository[],
  preferences: Record<string, RepoGuidanceSettings>,
  yaml: string,
): { handled: boolean; value?: unknown } {
  const match = url.pathname.match(/^\/api\/repos\/([^/]+)\/([^/]+)\/guidance(?:\/(file))?$/);
  if (!match || method !== 'GET') return { handled: false };
  const name = `${decodeURIComponent(match[1])}/${decodeURIComponent(match[2])}`;
  const repo = repos.find((repo) => repo.full_name === name);
  if (!repo) throw new Error('Repository not found.');
  const hasFiles = name === 'northstar/api';
  if (match[3]) {
    const path = url.searchParams.get('path') || '';
    if (!hasFiles || !Object.hasOwn(contents, path)) throw new Error('Guidance file not found.');
    return { handled: true, value: contents[path] };
  }
  const global = guidanceDefaults(yaml);
  const pref = preferences[name];
  return {
    handled: true,
    value: {
      full_name: name,
      default_branch: repo.default_branch,
      tree: 'b'.repeat(40),
      truncated_tree: false,
      global: { guidance: global.guidance, notes_chars: global.notes_chars },
      repo: { guidance: pref?.guidance ?? null, notes: pref?.notes ?? null },
      effective_guidance: pref?.guidance ?? global.guidance,
      max_bytes: global.max_bytes,
      files: hasFiles
        ? ['AGENTS.md', 'CLAUDE.md', 'src/server/AGENTS.md', 'docs/REVIEW.md'].map((path) => ({
            path,
            dir: path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '',
            kind: path.endsWith('REVIEW.md')
              ? 'review'
              : path.endsWith('AGENTS.md')
                ? 'agents'
                : 'claude',
            bytes: bytes(contents[path]),
          }))
        : [],
      files_omitted: 0,
      skills: hasFiles
        ? skillDescriptions.map(([name, description]) => ({
            name,
            description,
            path: `.agents/skills/${name}/SKILL.md`,
            bytes: bytes(contents[`.agents/skills/${name}/SKILL.md`]),
          }))
        : [],
      skills_omitted: 0,
    } satisfies GuidancePreview,
  };
}

export function applyMockGuidance(override: RepoGuidanceSettings, data: Record<string, unknown>) {
  if ('guidance' in data && data.guidance !== null && !isGuidanceMode(data.guidance))
    throw new Error('Invalid guidance level.');
  if ('notes' in data && data.notes !== null && typeof data.notes !== 'string')
    throw new Error('Notes must be a string or null.');
  const notes = typeof data.notes === 'string' ? data.notes.trim() || null : null;
  if (notes && notesCharacters(notes) > maxNotesCharacters)
    throw new Error('notes must be at most 8000 characters');
  if ('guidance' in data) override.guidance = data.guidance as RepoGuidanceSettings['guidance'];
  if ('notes' in data) override.notes = notes;
}
