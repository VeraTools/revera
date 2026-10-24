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
    /`([^`\n]+)`|\*\*([^*\n]+)\*\*|(?<!\w)_([^_\n]+)_(?!\w)|\[([^\[\]\n]+)\]\((https?:\/\/[^\s)]+)\)/g,
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

// GitHub bodies are data, never HTML. Tokenize raw text and escape each leaf;
// generated markup never re-enters the parser (including links and code).
function githubInline(text: string, depth = 0): string {
  if (depth > 8) return escape(text);
  const token =
    /`([^`\n]+)`|!\[([^\[\]\n]*)\]\((https?:\/\/[^\s)]+)\)|\[([^\[\]\n]+)\]\((https?:\/\/[^\s)]+)\)|\*\*([^*\n]+)\*\*|(?<!\w)(?:\*([^*\n]+)\*|_([^_\n]+)_)(?!\w)|https?:\/\/[^\s<>]+/g;
  let result = '',
    start = 0;
  for (const match of text.matchAll(token)) {
    result += escape(text.slice(start, match.index));
    const [, code, image, imageUrl, label, linkUrl, bold, starItalic, underscoreItalic] = match;
    if (code !== undefined) result += `<code>${escape(code)}</code>`;
    else if (image !== undefined || label !== undefined) {
      const href = imageUrl || linkUrl;
      result += `<a href="${escape(href)}" rel="noreferrer" target="_blank">${image !== undefined ? 'image' : githubInline(label, depth + 1)}</a>`;
    } else if (bold !== undefined) result += `<strong>${githubInline(bold, depth + 1)}</strong>`;
    else if (starItalic !== undefined || underscoreItalic !== undefined)
      result += `<em>${githubInline(starItalic ?? underscoreItalic, depth + 1)}</em>`;
    else {
      const href = match[0].replace(/[.,;:!?)\]]+$/, '');
      result += `<a href="${escape(href)}" rel="noreferrer" target="_blank">${escape(href)}</a>${escape(match[0].slice(href.length))}`;
    }
    start = match.index + match[0].length;
  }
  return result + escape(text.slice(start));
}

export function renderGithub(md: string): string {
  const lines = md.split(/\r?\n/);
  const blocks: string[] = [];
  let group: string[] = [],
    kind = '',
    fence: string[] | null = null,
    details = 0;
  let comment = false;
  function stripComments(line: string): string {
    const token = /`[^`\n]+`|<!--/g;
    let result = '',
      cursor = 0;
    while (cursor < line.length) {
      if (comment) {
        const end = line.indexOf('-->', cursor);
        if (end === -1) return result;
        cursor = end + 3;
        comment = false;
      } else {
        token.lastIndex = cursor;
        const next = token.exec(line);
        if (!next) return result + line.slice(cursor);
        result += line.slice(cursor, next.index);
        if (next[0] === '<!--') comment = true;
        else result += next[0]; // Inline code, including comment markers, stays literal.
        cursor = token.lastIndex;
      }
    }
    return result;
  }
  function flush() {
    if (!group.length) return;
    const content = group.map((line) => githubInline(line));
    blocks.push(
      kind === 'ul' || kind === 'ol'
        ? `<${kind}>${content.map((line) => `<li>${line}</li>`).join('')}</${kind}>`
        : kind === 'quote'
          ? `<blockquote><p>${content.join('<br>')}</p></blockquote>`
          : `<p>${content.join('<br>')}</p>`,
    );
    group = [];
  }
  for (const rawLine of lines) {
    const line = fence ? rawLine : stripComments(rawLine);
    if (/^\s*```/.test(line)) {
      flush();
      kind = '';
      if (fence) {
        blocks.push(`<pre><code>${escape(fence.join('\n'))}</code></pre>`);
        fence = null;
      } else fence = [];
      continue;
    }
    if (fence) {
      fence.push(line);
      continue;
    }
    const opening = line.match(/^\s*<details(?:\s+open)?>\s*(?:<summary>(.*?)<\/summary>)?\s*$/i);
    const summary = line.match(/^\s*<summary>(.*?)<\/summary>\s*$/i);
    if (opening || (summary && details)) {
      flush();
      kind = '';
      if (opening) {
        blocks.push('<details>');
        details++;
      }
      const label = opening?.[1] ?? summary?.[1];
      if (label !== undefined)
        blocks.push(`<summary>${escape(label.replace(/<[^>]*>/g, ''))}</summary>`);
      continue;
    }
    if (/^\s*<\/details>\s*$/i.test(line) && details) {
      flush();
      kind = '';
      blocks.push('</details>');
      details--;
      continue;
    }
    const heading = line.match(/^#{1,6}\s+(.+)$/);
    if (!line.trim() || heading || /^\s*(?:-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {
      flush();
      kind = '';
      if (heading) blocks.push(`<h4>${githubInline(heading[1])}</h4>`);
      else if (line.trim()) blocks.push('<hr>');
      continue;
    }
    const list = line.match(/^\s*(?:[-+*]|\d+[.)])\s+(.+)$/);
    const quote = line.match(/^>\s?(.*)$/);
    const next = list ? (/^\s*\d/.test(line) ? 'ol' : 'ul') : quote ? 'quote' : 'p';
    if (next !== kind) flush();
    kind = next;
    group.push(
      list
        ? list[1].replace(/^\[([ xX])\]\s*/, (_, checked) => (checked === ' ' ? '☐ ' : '☑ '))
        : quote
          ? quote[1]
          : line,
    );
  }
  flush();
  if (fence) blocks.push(`<pre><code>${escape(fence.join('\n'))}</code></pre>`);
  while (details-- > 0) blocks.push('</details>');
  return blocks.join('\n');
}
