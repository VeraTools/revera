export type Role = 'investigator' | 'validator' | 'embedding';
export type Trigger = 'mention' | 'auto' | 'off';
export type ReviewMode = 'quick' | 'standard' | 'deep';
export type JobStatus =
  'queued' | 'running' | 'complete' | 'partial' | 'failed' | 'cancelled' | 'skipped';
export type SetupStage = 'create_app' | 'sign_in' | 'install' | 'configure' | 'done';
export interface Session {
  authenticated: boolean;
  user?: { login: string; avatar_url: string };
  csrf?: string;
  setup: { stage: SetupStage; public_url: string; authorized: boolean };
}
export interface JobSummary {
  id: string;
  kind: 'review' | 'index';
  repo: string;
  pr?: number;
  title?: string;
  author?: string;
  head_sha?: string;
  trigger: 'mention' | 'auto' | 'manual' | 'rerun' | 'webhook' | 'reconcile';
  mode: ReviewMode;
  status: JobStatus;
  reason?: string;
  findings: { total: number; high: number; medium: number; low: number };
  queued_at: string;
  started_at?: string;
  finished_at?: string;
  duration_ms?: number;
  tokens?: { prompt: number; completion: number; cached: number };
  check_run_url?: string;
  pr_url?: string;
}
export interface Finding {
  severity: 'high' | 'medium' | 'low';
  file: string;
  line?: number;
  title: string;
  body?: string;
  status: string;
}
export interface JobDetail {
  job: JobSummary;
  summary_markdown?: string;
  findings: Finding[];
  strategy?: string;
  stats?: {
    candidates?: number;
    accepted?: number;
    rejected?: number;
    files_read?: number;
    retrieval?: string;
    [key: string]: unknown;
  };
  timing?: {
    total_ms?: number;
    vera_index_ms?: number;
    lanes_ms?: number;
    validate_ms?: number;
    validate_p95_ms?: number;
    [key: string]: unknown;
  };
  ledger?: {
    by_route?: {
      role: string;
      model: string;
      effective_reasoning?: string;
      requests: number;
      prompt_tokens: number;
      completion_tokens: number;
      cached_prompt_tokens: number;
      [key: string]: unknown;
    }[];
    fallback_events?: { role: string; from_model: string; to_model: string; reason: string }[];
    unavailable_routes?: { role: string; model: string; reason: string }[];
    [key: string]: unknown;
  };
}
export interface OverviewWindow {
  reviews: number;
  findings: number;
  failed: number;
  severity: { high: number; medium: number; low: number };
  tokens: number;
  prompt_tokens: number;
  cached_tokens: number;
  median_duration_ms: number | null;
}
export interface Overview {
  app: {
    name: string;
    slug: string;
    html_url: string;
    install_url: string;
    owner: string;
    issue_comment_subscribed: boolean | null;
    settings_url: string;
  };
  health: {
    github: { ok: boolean; error?: string };
    models: { role: string; model: string; ok?: boolean; checked_at?: string; error?: string }[];
    vera: { enabled: boolean; version: string };
  };
  queue: { running: JobSummary[]; queued: JobSummary[] };
  last_24h: OverviewWindow;
  last_7d: OverviewWindow;
  history_since: string | null;
  recent: JobSummary[];
}
export interface Repository {
  full_name: string;
  private: boolean;
  default_branch: string;
  trigger: Trigger;
  mode: ReviewMode;
  trigger_override: Trigger | null;
  mode_override: ReviewMode | null;
  has_override: boolean;
  pushed_at?: string;
  open_prs?: number;
  review_count: number;
  finding_count: number;
  index: { state: 'none' | 'ready' | 'building'; updated_at?: string; retry_at?: string };
  last_job?: JobSummary;
  last_review?: JobSummary;
  html_url: string;
}
export interface PullRequest {
  number: number;
  title: string;
  author: string;
  draft: boolean;
  head_sha: string;
  updated_at: string;
  last_job?: JobSummary;
}
export interface ServerSettings {
  admins: string[];
  allowed_owners: string[];
  default_trigger: Trigger;
  default_mode: ReviewMode;
  repos: Record<string, { trigger?: Trigger | null; mode?: ReviewMode | null }>;
  review_drafts: boolean;
  skip_authors: string[];
}
export interface Secret {
  name: string;
  source: 'dashboard' | 'environment' | 'missing';
  environment: boolean;
}
export interface Settings {
  yaml: string;
  server: ServerSettings;
  secrets: Secret[];
  referenced_env: string[];
}
export interface TestResult {
  ok: boolean;
  latency_ms: number;
  error?: string;
}
export interface JobsPage {
  jobs: JobSummary[];
  next?: string;
}

// Connected GitHub accounts (installation does not imply deployment approval).
export interface Installation {
  id: number | null;
  account: { login: string; id: number | null; type: string };
  repository_selection: string | null;
  suspended: boolean;
  permissions: Record<string, string>;
  html_url: string;
  status: 'allowed' | 'awaiting_approval' | 'permission_update' | 'suspended' | 'not_installed';
  allowed: boolean;
  repo_count?: number;
  error?: string;
}
export interface Installations {
  installations: Installation[];
  install_url: string;
  settings_url: string;
  advanced_url: string;
  requested_permissions: Record<string, string>;
}
