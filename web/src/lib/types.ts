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
  timing?: Record<string, unknown>;
  ledger?: {
    fallback_events?: { role: string; from_model: string; to_model: string; reason: string }[];
    unavailable_routes?: { role: string; model: string; reason: string }[];
    [key: string]: unknown;
  };
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
  last_24h: { reviews: number; findings: number; failed: number; tokens: number };
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
  index: { state: 'none' | 'ready' | 'building'; updated_at?: string };
  last_job?: JobSummary;
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
