import { parseDocument } from 'yaml';
import type { GuidanceFile, GuidanceMode } from './types';

export const maxNotesCharacters = 8000;
export const notesCharacters = (text: string) => [...text].length;
export const guidanceLevels: { value: GuidanceMode; label: string }[] = [
  { value: 'off', label: 'Off' },
  { value: 'review', label: 'Review files (REVIEW.md)' },
  { value: 'agents', label: 'Agent files (REVIEW.md, else AGENTS.md, else CLAUDE.md)' },
  { value: 'skills', label: 'Agent files + skills' },
];
export const guidanceHint: Record<GuidanceMode, string> = {
  off: 'No repository files or skills are read. Operator review notes still apply.',
  review: 'Read REVIEW.md in the root and in directories containing changed files.',
  agents: 'Read the first available REVIEW.md, AGENTS.md or CLAUDE.md in each relevant directory.',
  skills: 'Read agent files and list project skills, which reviewers can open when relevant.',
};
export function isGuidanceMode(value: unknown): value is GuidanceMode {
  return guidanceLevels.some((level) => level.value === value);
}
export function guidanceDefaults(yaml: string) {
  const doc = parseDocument(yaml);
  if (doc.errors.length) throw new Error(doc.errors[0].message);
  const mode = doc.getIn(['review', 'guidance']);
  const notes = doc.getIn(['review', 'notes']);
  const maxBytes = doc.getIn(['review', 'guidance_max_bytes']);
  return {
    guidance: isGuidanceMode(mode) ? mode : ('off' as GuidanceMode),
    notes_chars: notesCharacters(typeof notes === 'string' ? notes : ''),
    max_bytes: typeof maxBytes === 'number' ? maxBytes : 16384,
  };
}
export function guidanceGroups(files: GuidanceFile[], mode: GuidanceMode) {
  const priority =
    mode === 'off' ? [] : mode === 'review' ? ['review'] : ['review', 'agents', 'claude'];
  const dirs = [...new Set(files.map((file) => file.dir))].sort();
  return dirs.map((dir) => {
    const group = files
      .filter((file) => file.dir === dir)
      .sort(
        (a, b) =>
          ['review', 'agents', 'claude'].indexOf(a.kind) -
          ['review', 'agents', 'claude'].indexOf(b.kind),
      );
    const winner = priority.map((kind) => group.find((file) => file.kind === kind)).find(Boolean);
    return {
      dir,
      files: group.map((file) => ({
        ...file,
        used: file === winner,
        reason:
          file === winner
            ? 'Eligible'
            : !priority.includes(file.kind) || !winner
              ? 'not read at this level'
              : `${winner.path.split('/').at(-1)} takes priority`,
      })),
    };
  });
}
export const guidanceSize = (bytes: number) =>
  bytes < 1024 ? `${bytes} B` : `${Number((bytes / 1024).toFixed(1))} KB`;
