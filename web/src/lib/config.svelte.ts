import { parseDocument, isMap, isSeq } from 'yaml';

export type ConfigPath = (string | number)[];
export const triggers = [
  { value: 'mention', label: 'Mention' },
  { value: 'auto', label: 'Auto' },
  { value: 'off', label: 'Off' },
];
export const modes = [
  { value: 'quick', label: 'Quick' },
  { value: 'standard', label: 'Standard' },
  { value: 'deep', label: 'Deep' },
];

export const protocols = [
  { value: 'openai-chat', label: 'Chat completions' },
  { value: 'openai-responses', label: 'Responses' },
  { value: 'anthropic', label: 'Messages' },
  { value: 'gemini', label: 'Generate content' },
  { value: 'scripted', label: 'Scripted (local transcript)' },
];
export const efforts = ['none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max'];
export const backends = [
  { value: 'local', label: 'Local (on-device)' },
  { value: 'api', label: 'API endpoint' },
];

export class ConfigDocument {
  private document = parseDocument('');
  private revision = $state(0);
  yaml = $state('');
  errors = $state<string[]>([]);
  constructor(yaml: string) {
    this.replace(yaml);
  }
  replace(yaml: string) {
    this.document = parseDocument(yaml);
    this.yaml = yaml;
    this.errors = this.document.errors.map((error) => error.message);
    this.revision++;
  }
  get(path: ConfigPath, fallback: string): string;
  get(path: ConfigPath, fallback: number): number;
  get(path: ConfigPath, fallback: boolean): boolean;
  get<T>(path: ConfigPath, fallback: T): T;
  get<T>(path: ConfigPath, fallback: T): T {
    this.revision;
    let value = this.document.getIn(path);
    if (
      value === undefined &&
      path[0] === 'models' &&
      path[1] === 'validator' &&
      !this.document.hasIn(['models', 'validator'])
    )
      value = this.document.getIn(['models', 'investigator', ...path.slice(2)]);
    if (value && typeof value === 'object' && 'toJSON' in value)
      value = (value as { toJSON: () => unknown }).toJSON();
    return value === undefined || value === null ? fallback : (value as T);
  }
  has(path: ConfigPath) {
    this.revision;
    return this.document.hasIn(path);
  }
  set(path: ConfigPath, value: unknown) {
    if (this.errors.length) return;
    if (
      path[0] === 'models' &&
      ['investigator', 'validator'].includes(String(path[1])) &&
      !this.document.hasIn(['models', 'investigator'])
    ) {
      this.document.setIn(
        ['models', 'investigator'],
        this.document.createNode({
          protocol: 'openai-chat',
          base_url: '',
          api_key_env: 'REVERA_MODEL_API_KEY',
          model: '',
        }),
      );
    }
    if (
      path[0] === 'models' &&
      path[1] === 'validator' &&
      !this.document.hasIn(['models', 'validator'])
    ) {
      const investigator = this.document.getIn(['models', 'investigator']);
      this.document.setIn(
        ['models', 'validator'],
        isMap(investigator) ? investigator.clone() : this.document.createNode({}),
      );
    }
    // New collections must be YAML nodes so subsequent nested form edits work.
    // Leave scalar updates as values to preserve their existing comments.
    this.document.setIn(
      path,
      value !== null && typeof value === 'object' ? this.document.createNode(value) : value,
    );
    if (path[0] === 'models' && ['investigator', 'validator'].includes(String(path[1]))) {
      const investigator = this.document.getIn(['models', 'investigator']);
      const validator = this.document.getIn(['models', 'validator']);
      if (
        isMap(investigator) &&
        isMap(validator) &&
        JSON.stringify(investigator.toJSON()) === JSON.stringify(validator.toJSON())
      ) {
        this.document.deleteIn(['models', 'validator']);
      }
    }
    this.yaml = this.document.toString();
    this.revision++;
  }
  remove(path: ConfigPath) {
    if (this.errors.length) return;
    this.document.deleteIn(path);
    this.yaml = this.document.toString();
    this.revision++;
  }
  private fallbackList(role: string) {
    if (this.errors.length) return;
    const path = ['models', role, 'fallback'];
    if (role === 'validator' && !this.has(['models', role])) {
      const route = this.document.getIn(['models', 'investigator']);
      if (isMap(route)) this.document.setIn(['models', role], route.clone());
    }
    if (!this.has(path)) this.set(path, []);
    const node = this.document.getIn(path);
    return isSeq(node) ? node : undefined;
  }
  addFallback(role: string) {
    const route = this.get<Record<string, unknown>>(['models', role], {
      protocol: 'openai-chat',
      base_url: '',
      api_key_env: 'REVERA_MODEL_API_KEY',
    });
    const { fallback: _fallback, ...fields } = route;
    const routes = this.fallbackList(role);
    if (!routes) return;
    routes.add(this.document.createNode({ ...fields, model: '' }));
    this.yaml = this.document.toString();
    this.revision++;
  }
  changeFallback(role: string, index: number, direction?: -1 | 1) {
    const routes = this.fallbackList(role);
    if (!routes || index < 0 || index >= routes.items.length) return;
    if (direction) {
      const target = index + direction;
      if (target < 0 || target >= routes.items.length) return;
      [routes.items[index], routes.items[target]] = [routes.items[target], routes.items[index]];
    } else {
      routes.items.splice(index, 1);
      if (!routes.items.length) this.document.deleteIn(['models', role, 'fallback']);
    }
    this.yaml = this.document.toString();
    this.revision++;
  }
  setRerankerEnabled(enabled: boolean, useEmbedding = false) {
    if (!enabled) {
      this.remove(['vera', 'reranker']);
      return;
    }
    this.set(['vera', 'reranker'], {
      base_url: useEmbedding
        ? this.get(['vera', 'embedding', 'base_url'], '')
        : 'https://api.example.com/v1',
      model: useEmbedding ? '' : 'model-name',
      api_key_env: useEmbedding
        ? this.get(['vera', 'embedding', 'api_key_env'], 'EMBEDDING_API_KEY')
        : 'RERANKER_API_KEY',
    });
  }
  setRerankerFormat(value: string) {
    if (value) this.set(['vera', 'reranker', 'protocol'], value);
    else this.remove(['vera', 'reranker', 'protocol']);
  }
  effort(path: ConfigPath) {
    const value = this.get<unknown>([...path, 'reasoning'], 'medium');
    return value && typeof value === 'object'
      ? String((value as Record<string, unknown>).effort ?? 'medium')
      : String(value);
  }
  setEffort(path: ConfigPath, value: string) {
    const reasoning = this.get<unknown>([...path, 'reasoning'], 'medium');
    this.set(
      reasoning && typeof reasoning === 'object'
        ? [...path, 'reasoning', 'effort']
        : [...path, 'reasoning'],
      value,
    );
  }
  referencedEnv() {
    this.revision;
    const names = new Set<string>();
    const walk = (value: unknown, key = '') => {
      if (typeof value === 'string') {
        if (key === 'api_key_env' && value) names.add(value);
        for (const match of value.matchAll(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) names.add(match[1]);
      } else if (Array.isArray(value)) value.forEach((item) => walk(item));
      else if (value && typeof value === 'object')
        Object.entries(value).forEach(([name, item]) => walk(item, name));
    };
    try {
      walk(this.document.toJS());
    } catch {
      /* Invalid YAML is reported by the editor. */
    }
    return [...names].sort();
  }
}
