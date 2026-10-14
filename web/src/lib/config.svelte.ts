import { parseDocument, isMap } from 'yaml';

export const protocols = [
  { value: 'openai-chat', label: 'Chat completions' },
  { value: 'openai-responses', label: 'Responses' },
  { value: 'anthropic', label: 'Messages' },
  { value: 'gemini', label: 'Generate content' },
  { value: 'scripted', label: 'Scripted (local transcript)' }
];
export const efforts = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
export const backends = [
  { value: 'local', label: 'Local (on-device)' },
  { value: 'api', label: 'API endpoint' }
];

export class ConfigDocument {
  private document = parseDocument('');
  private revision = $state(0);
  yaml = $state('');
  errors = $state<string[]>([]);
  constructor(yaml: string) { this.replace(yaml); }
  replace(yaml: string) {
    this.document = parseDocument(yaml);
    this.yaml = yaml;
    this.errors = this.document.errors.map((error) => error.message);
    this.revision++;
  }
  get(path: string[], fallback: string): string;
  get(path: string[], fallback: number): number;
  get(path: string[], fallback: boolean): boolean;
  get<T>(path: string[], fallback: T): T;
  get<T>(path: string[], fallback: T): T {
    this.revision;
    let value = this.document.getIn(path);
    if (value === undefined && path[0] === 'models' && path[1] === 'validator') value = this.document.getIn(['models', 'investigator', ...path.slice(2)]);
    if (value && typeof value === 'object' && 'toJSON' in value) value = (value as { toJSON: () => unknown }).toJSON();
    return value === undefined || value === null ? fallback : value as T;
  }
  has(path: string[]) { this.revision; return this.document.hasIn(path); }
  set(path: string[], value: unknown) {
    if (this.errors.length) return;
    if (path[0] === 'models' && path[1] === 'validator' && !this.document.hasIn(['models', 'validator'])) {
      const investigator = this.document.getIn(['models', 'investigator']);
      this.document.setIn(['models', 'validator'], isMap(investigator) ? investigator.clone() : {});
    }
    // New collections must be YAML nodes so subsequent nested form edits work.
    // Leave scalar updates as values to preserve their existing comments.
    this.document.setIn(path, value !== null && typeof value === 'object' ? this.document.createNode(value) : value);
    this.yaml = this.document.toString();
    this.revision++;
  }
  remove(path: string[]) { if (this.errors.length) return; this.document.deleteIn(path); this.yaml = this.document.toString(); this.revision++; }
  effort(path: string[]) { const value = this.get<unknown>([...path, 'reasoning'], 'medium'); return value && typeof value === 'object' ? String((value as Record<string, unknown>).effort ?? 'medium') : String(value); }
  setEffort(path: string[], value: string) {
    const reasoning = this.get<unknown>([...path, 'reasoning'], 'medium');
    this.set(reasoning && typeof reasoning === 'object' ? [...path, 'reasoning', 'effort'] : [...path, 'reasoning'], value);
  }
  referencedEnv() {
    this.revision;
    const names = new Set<string>();
    const walk = (value: unknown, key = '') => {
      if (typeof value === 'string') {
        if (key === 'api_key_env' && value) names.add(value);
        for (const match of value.matchAll(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) names.add(match[1]);
      } else if (Array.isArray(value)) value.forEach((item) => walk(item));
      else if (value && typeof value === 'object') Object.entries(value).forEach(([name, item]) => walk(item, name));
    };
    try { walk(this.document.toJS()); } catch { /* Invalid YAML is reported by the editor. */ }
    return [...names].sort();
  }
}
