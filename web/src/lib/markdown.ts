const escape = (text: string) =>
  text.replace(
    /[&<>"']/g,
    (char) =>
      ({
        '&': '&amp;',
        '<': '&lt;',
        '>': '&gt;',
        '"': '&quot;',
        "'": '&#39;',
      })[char]!,
  );

// Parse only escaped text; generated tags are never fed back into the parser.
function inline(text: string, depth = 0): string {
  if (depth > 8) return text;
  return text.replace(
    /`([^`\n]+)`|\*\*([^*\n]+)\*\*|(?<!\w)_([^_\n]+)_(?!\w)|\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g,
    (_match, code, bold, italic, label, href) => {
      if (code !== undefined) return `<code>${code}</code>`;
      if (bold !== undefined) return `<strong>${inline(bold, depth + 1)}</strong>`;
      if (italic !== undefined) return `<em>${inline(italic, depth + 1)}</em>`;
      return `<a href="${href}" rel="noreferrer" target="_blank">${inline(label, depth + 1)}</a>`;
    },
  );
}

export function renderSummary(md: string): string {
  const lines = escape(md)
    .replace(/^\s*## Revera review\s*(?:\n|$)/, '')
    .split(/\r?\n/);
  const blocks: string[] = [];
  let group: string[] = [];
  let kind = '';
  function flush() {
    if (!group.length) return;
    const content = group.map((line) => inline(line));
    blocks.push(
      kind === 'ul'
        ? `<ul>${content.map((line) => `<li>${line}</li>`).join('')}</ul>`
        : kind === 'quote'
          ? `<blockquote><p>${content.join(' ')}</p></blockquote>`
          : `<p>${content.join(' ')}</p>`,
    );
    group = [];
  }
  for (const line of lines) {
    const heading = line.match(/^(#{2,3}) (.+)$/);
    const footer = line.match(/^&lt;sub&gt;(.*)&lt;\/sub&gt;$/);
    if (!line.trim() || heading || footer) {
      flush();
      kind = '';
      if (heading) {
        const tag = heading[1].length === 2 ? 'h3' : 'h4';
        blocks.push(`<${tag}>${inline(heading[2])}</${tag}>`);
      }
      if (footer) blocks.push(`<p class="summary-footer">${inline(footer[1])}</p>`);
      continue;
    }
    if (kind === 'ul' && /^\s+\S/.test(line)) {
      group[group.length - 1] += ` ${line.trim()}`;
      continue;
    }
    const next = line.startsWith('- ') ? 'ul' : line.startsWith('&gt; ') ? 'quote' : 'p';
    if (next !== kind) flush();
    kind = next;
    group.push(next === 'ul' ? line.slice(2) : next === 'quote' ? line.slice(5) : line);
  }
  flush();
  return blocks.join('\n');
}
