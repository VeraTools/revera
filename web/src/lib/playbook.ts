// Skill front matter uses single-line values. Keep the markdown body untouched.
const frontMatter = /^---[^\S\r\n]*\r?\n([\s\S]*?)\r?\n---[^\S\r\n]*(?:\r?\n|$)/;
export const playbookBytes = (text: string) => new TextEncoder().encode(text).length;

export function skillMetadata(content: string) {
  const match = frontMatter.exec(content);
  if (!match) throw new Error('Bad front matter: start the skill with --- and close it with ---.');
  const fields: Record<string, string> = {};
  for (const line of match[1].split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith('#')) continue;
    const field = /^(name|description|enabled):\s*(.*?)\s*$/.exec(line);
    if (!field || Object.hasOwn(fields, field[1]))
      throw new Error('Bad front matter: use name, description and optional enabled once each.');
    let value = field[2];
    if (value.startsWith('"')) {
      try {
        value = JSON.parse(value);
      } catch {
        throw new Error('Bad front matter: invalid quoted value.');
      }
    } else if (value.startsWith("'")) {
      if (!value.endsWith("'")) throw new Error('Bad front matter: invalid quoted value.');
      value = value.slice(1, -1).replaceAll("''", "'");
    }
    fields[field[1]] = value;
  }
  if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(fields.name || ''))
    throw new Error(
      'Invalid name: use 1–40 lowercase letters, digits or hyphens; start with a letter or digit.',
    );
  if (
    !fields.description?.trim() ||
    /[\r\n]/.test(fields.description) ||
    [...fields.description].length > 300
  )
    throw new Error('Bad front matter: description must be one line, 1–300 characters.');
  if (fields.enabled !== undefined && !['true', 'false'].includes(fields.enabled))
    throw new Error('Bad front matter: enabled must be true or false.');
  return {
    name: fields.name,
    description: fields.description,
    enabled: fields.enabled !== 'false',
    body: content.slice(match[0].length),
  };
}

export function setSkillEnabled(content: string, enabled: boolean) {
  skillMetadata(content);
  return content.replace(frontMatter, (header, fields: string) => {
    const newline = header.includes('\r\n') ? '\r\n' : '\n';
    const lines = fields.split(/\r?\n/).filter((line) => !/^enabled:/.test(line));
    if (!enabled) lines.push('enabled: false');
    return `---${newline}${lines.join(newline)}${newline}---${header.endsWith('\n') ? newline : ''}`;
  });
}

// Match the core's effective-content limit, not the full Markdown file size.
export function effectiveSkillBytes(content: string) {
  const skill = skillMetadata(content);
  return skill.enabled
    ? playbookBytes(skill.name) + playbookBytes(skill.description) + playbookBytes(skill.body)
    : 0;
}

export function newSkillTemplate(name: string) {
  return `---\nname: ${name}\ndescription: Describe the defects this skill looks for.\n---\n\n## Applies to\n\nChanges that touch…\n\n## Check\n\n- Trace the changed behavior and its callers.\n\n## Evidence\n\nName the trigger, impact and source lines.\n\n## Not a defect\n\nDescribe cases that should not be reported.\n`;
}
