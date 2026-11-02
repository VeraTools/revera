import reviewer from '../../../playbook/reviewer.md?raw';
import logic from '../../../playbook/skills/logic.md?raw';
import contracts from '../../../playbook/skills/contracts.md?raw';
import state from '../../../playbook/skills/state.md?raw';
import errors from '../../../playbook/skills/errors.md?raw';
import concurrency from '../../../playbook/skills/concurrency.md?raw';
import security from '../../../playbook/skills/security.md?raw';
import filesystem from '../../../playbook/skills/filesystem.md?raw';
import subprocess from '../../../playbook/skills/subprocess.md?raw';
import resources from '../../../playbook/skills/resources.md?raw';
import config from '../../../playbook/skills/config.md?raw';
import tests from '../../../playbook/skills/tests.md?raw';
import docs from '../../../playbook/skills/docs.md?raw';
import { ApiError } from './api';
import { effectiveSkillBytes, playbookBytes, skillMetadata } from './playbook';
import type { Playbook, PlaybookSkill } from './types';

let current: Omit<Playbook, 'digest'> = {
  limits: {
    reviewer_max_bytes: 16384,
    skill_max_bytes: 8192,
    total_max_bytes: 65536,
    max_skills: 24,
  },
  reviewer: { builtin: reviewer, override: null, effective: reviewer },
  skills: [
    logic,
    contracts,
    state,
    errors,
    concurrency,
    security,
    filesystem,
    subprocess,
    resources,
    config,
    tests,
    docs,
  ].map((effective) => {
    const { name, description, enabled } = skillMetadata(effective);
    return {
      name,
      description,
      enabled,
      builtin: effective,
      override: null,
      effective,
      custom: false,
    };
  }),
  total_bytes: 0,
};
const totalBytes = (book: typeof current) =>
  playbookBytes(book.reviewer.effective) +
  book.skills.reduce((total, skill) => total + effectiveSkillBytes(skill.effective), 0);
current.total_bytes = totalBytes(current);

async function response(): Promise<Playbook> {
  const book = structuredClone(current);
  // The digest is opaque to the dashboard; use a stable content hash in the mock.
  const content = JSON.stringify([
    book.reviewer.effective,
    book.skills
      .filter((skill) => skill.enabled)
      .map((skill) => [skill.name, skill.description, skillMetadata(skill.effective).body]),
  ]);
  const hash = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(content));
  const digest = Array.from(new Uint8Array(hash), (byte) => byte.toString(16).padStart(2, '0'))
    .join('')
    .slice(0, 16);
  return { ...book, digest };
}

export async function mockPlaybookRequest(
  path: string,
  method: string,
  data: Record<string, unknown> | undefined,
  secrets: Iterable<string>,
): Promise<Playbook> {
  if (path === '/api/playbook' && method === 'GET') return response();
  const skillPath = /^\/api\/playbook\/skills\/([^/]+)$/.exec(path);
  if ((path !== '/api/playbook/reviewer' && !skillPath) || !['PUT', 'DELETE'].includes(method))
    throw new ApiError('Playbook endpoint not found.', 404);
  const draft = structuredClone(current);
  try {
    const name = skillPath ? decodeURIComponent(skillPath[1]) : '';
    if (skillPath && !/^[a-z0-9][a-z0-9-]{0,39}$/.test(name)) throw new Error('Invalid name.');
    const content = data?.content;
    if (method === 'PUT') {
      if (typeof content !== 'string') throw new Error('Content must be a string.');
      if (
        playbookBytes(content) >
        (skillPath ? draft.limits.skill_max_bytes : draft.limits.reviewer_max_bytes)
      )
        throw new Error('Content is over size limit.');
      for (const secret of secrets)
        if (secret && content.includes(secret))
          throw new Error('content contains a registered secret');
    }
    if (!skillPath) {
      draft.reviewer.override =
        method === 'DELETE' || content === draft.reviewer.builtin ? null : (content as string);
      draft.reviewer.effective = draft.reviewer.override ?? draft.reviewer.builtin;
    } else {
      const index = draft.skills.findIndex((skill) => skill.name === name);
      const previous = draft.skills[index];
      if (method === 'DELETE' && !previous) throw new ApiError('Skill not found.', 404);
      if (method === 'DELETE' && previous.custom) draft.skills.splice(index, 1);
      else {
        const effective = method === 'DELETE' ? previous.builtin! : (content as string);
        const metadata = skillMetadata(effective);
        if (metadata.name !== name)
          throw new Error('Name mismatch: front matter name must match the skill name.');
        const skill: PlaybookSkill = {
          name,
          description: metadata.description,
          enabled: metadata.enabled,
          builtin: previous?.builtin ?? null,
          override: method === 'DELETE' || effective === previous?.builtin ? null : effective,
          effective,
          custom: previous?.custom ?? true,
        };
        if (previous) draft.skills[index] = skill;
        else draft.skills.push(skill);
      }
    }
    draft.skills = [
      ...draft.skills.filter((skill) => !skill.custom),
      ...draft.skills
        .filter((skill) => skill.custom)
        .sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0)),
    ];
    if (draft.skills.length > draft.limits.max_skills) throw new Error('Too many skills.');
    draft.total_bytes = totalBytes(draft);
    if (draft.total_bytes > draft.limits.total_max_bytes)
      throw new Error('Playbook total is over size limit.');
  } catch (cause) {
    if (cause instanceof ApiError) throw cause;
    throw new ApiError(cause instanceof Error ? cause.message : 'Invalid playbook.', 400);
  }
  current = draft;
  return response();
}
