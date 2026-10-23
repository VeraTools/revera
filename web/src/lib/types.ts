export type Role = 'investigator' | 'validator' | 'embedding' | 'fixer' | 'fix_verifier';
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
  source: 'installed' | 'external';
  publication: 'github' | 'private';
  repo_id?: number;
  base_sha?: string;
  id: string;
  kind: 'review' | 'index';
  repo: string;
  pr?: number;
  title?: string;
  author?: string;
  head_sha?: string;
  trigger:
    'mention' | 'auto' | 'manual' | 'rerun' | 'webhook' | 'reconcile' | 'tracking' | 'pull_request';
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
  id: string;
  defect_key: string;
  severity: 'high' | 'medium' | 'low';
  file: string;
  start_line: number;
  end_line: number;
  title: string;
  claim: string;
  trigger: string;
  impact: string;
  supporting_evidence: { path: string; start_line: number; end_line: number; note: string }[];
  validation_status: 'accepted' | null;
  carried?: boolean;
  validated_fix?: string | null;
  validated_fix_label: string;
  suggestion?: {
    start_line: number;
    end_line: number;
    original: string;
    replacement: string;
  } | null;
  reviewed_head_sha: string;
  reviewed_base_sha: string;
  html_url?: string | null;
  prompt: string;
}
export interface Diagnostic {
  id: string;
  status: 'rejected' | 'uncertain' | 'unvalidated';
  severity: string;
  title: string;
  file: string;
  start_line?: number;
  rationale: string;
}
export interface JobDetail {
  job: JobSummary;
  summary_markdown?: string;
  findings: Finding[];
  carried?: Finding[];
  counts: JobSummary['findings'];
  instructions: string;
  reviewed_head_sha?: string;
  reviewed_base_sha?: string;
  stale: boolean | null;
  diagnostics?: Diagnostic[];
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
export interface Repository extends RepositoryGuidanceSummary {
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
  external: ExternalPolicy;
  admins: string[];
  allowed_owners: string[];
  default_trigger: Trigger;
  default_mode: ReviewMode;
  repos: Record<
    string,
    { trigger?: Trigger | null; mode?: ReviewMode | null } & RepoGuidanceSettings
  >;
  review_drafts: boolean;
  index_on_push: boolean;
  skip_authors: string[];
}

// External public PR reviews are always private; tracking is affirmative opt-in.
export interface ExternalPolicy {
  tracking_enabled: boolean;
  poll_minutes: number;
  max_tracked: number;
  max_repo_size_kb: number;
  reviews_per_hour: number;
  concurrency: number;
  retention_days: number;
  max_checkouts: number;
}
export interface ExternalPR {
  url: string;
  repo: string;
  number: number;
  title: string;
}
export interface TrackedPR extends ExternalPR {
  repo_id: number;
  actor: string;
  head_sha: string;
  mode: ReviewMode;
  status: 'tracking' | 'stopped';
  reason: string | null;
  next_poll: number;
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

// Fixes are separate from review jobs and never imply GitHub write permission.
export type FixState =
  | 'awaiting_confirmation'
  | 'queued'
  | 'generating'
  | 'validating'
  | 'ready'
  | 'publishing'
  | 'published'
  | 'verifying'
  | 'verified'
  | 'failed'
  | 'validation_failed'
  | 'publish_failed'
  | 'cancelled'
  | 'discarded'
  | 'stale'
  | 'unsupported';
export type FixDelivery = 'patch' | 'draft_pr' | 'same_branch';
export interface FixCapability {
  mode: FixDelivery;
  available: boolean;
  reason: string;
}
export interface FixLimits {
  attempts: number;
  tool_calls: number;
  seconds: number;
  model_requests: number;
  max_files: number;
  max_patch_kb: number;
  concurrent: number;
  per_user_hour: number;
  per_repo_day: number;
}
export interface FixPolicy {
  offer: boolean;
  execution: boolean;
  trigger: 'manual' | 'after_review';
  confirm: boolean;
  trusted_skip_confirm: boolean;
  delivery: FixDelivery;
  allow_same_branch: boolean;
  require_approval: boolean;
  requesters: 'maintainers' | 'writers';
  author_preview: boolean;
  allow_workflow_changes: boolean;
  external: boolean;
  rereview: boolean;
  branch_prefix: string;
  test_command: string[];
  test_timeout_seconds: number;
  limits: FixLimits;
}
export interface FixOverride {
  execution?: boolean | null;
  trigger?: FixPolicy['trigger'] | null;
  delivery?: FixDelivery | null;
}
export interface SandboxStatus {
  available: boolean;
  proc: boolean;
  reason: string;
}
export interface FixSettings {
  fixes: FixPolicy;
  repos: Record<string, FixOverride>;
  sandbox: SandboxStatus;
  models: { fixer: string; fix_verifier: string };
}
export type FixFinding = Pick<
  Finding,
  'id' | 'severity' | 'title' | 'file' | 'start_line' | 'end_line'
>;
export interface FixRecord {
  id: string;
  created_at: string;
  updated_at: string;
  state: FixState;
  reason: string | null;
  source: 'dashboard' | 'mention' | 'check' | 'auto';
  requested_by: string;
  consent: 'persistent' | 'once' | null;
  repo: string;
  repo_source: 'installed' | 'external';
  pr: number;
  pr_url: string;
  pr_title: string;
  review_job: string;
  base_sha: string;
  head_sha: string;
  findings: FixFinding[];
  attempt: number;
  max_attempts: number;
  progress: string | null;
  generation: {
    stopped: string;
    summary: string;
    per_finding: { id: string; disposition: 'fixed' | 'partial' | 'not_fixed'; note: string }[];
  } | null;
  verification: {
    per_finding: {
      id: string;
      verdict: 'resolved' | 'unresolved' | 'uncertain';
      rationale: string;
    }[];
    new_problems: string[];
  } | null;
  tests: {
    outcome: 'not_configured' | 'passed' | 'failed' | 'timed_out' | 'unavailable';
    output: string;
  } | null;
  patch: {
    files: {
      path: string;
      status: 'modified' | 'added' | 'deleted';
      additions: number;
      deletions: number;
    }[];
    bytes: number;
    warnings: string[];
    errors: string[];
  } | null;
  usage: {
    requests: number;
    prompt_tokens: number;
    completion_tokens: number;
    routes: string[];
  } | null;
  delivery: {
    mode: 'draft_pr' | 'same_branch';
    branch: string;
    commit: string;
    pr: number;
    url: string;
    error: string;
  } | null;
  verify_review: {
    job: string;
    state: string;
    resolved: string[];
    still_open: string[];
    new_findings: number;
    note: string;
  } | null;
  events: { at: string; actor: string; action: string; detail: string }[];
}
export type FixSummary = Omit<
  FixRecord,
  'generation' | 'verification' | 'tests' | 'patch' | 'events'
> & {
  files: number;
  resolved: number;
  findings_count: number;
};
export type FixAction = 'confirm' | 'approve' | 'regenerate' | 'cancel' | 'discard';
export interface FixDetail {
  fix: FixRecord;
  patch: string;
  delivery: FixCapability[];
  actions: Record<FixAction, boolean>;
  reasons: Partial<Record<FixAction, string>>;
}
export interface FixPlan {
  job: JobSummary;
  findings: Finding[];
  execution_enabled: boolean;
  consent_required: boolean;
  confirm_required: boolean;
  sandbox: SandboxStatus;
  limits: FixLimits;
  estimate: { max_model_requests: number; max_seconds: number; attempts: number };
  delivery: FixCapability[];
  existing: FixSummary | null;
  blocked: string | null;
}

// Repository review guidance (base-commit content; operator notes are separate).
export type GuidanceMode = 'off' | 'review' | 'agents' | 'skills';
export interface RepositoryGuidanceSummary {
  guidance: GuidanceMode | null;
  has_notes: boolean;
}
export interface RepoGuidanceSettings {
  guidance?: GuidanceMode | null;
  notes?: string | null;
}
export interface GuidanceFile {
  path: string;
  dir: string;
  kind: 'review' | 'agents' | 'claude';
  bytes: number;
}
export interface GuidanceSkill {
  name: string;
  path: string;
  description: string;
  bytes: number;
}
export interface GuidancePreview {
  full_name: string;
  default_branch: string;
  tree: string;
  truncated_tree: boolean;
  files: GuidanceFile[];
  skills: GuidanceSkill[];
  skills_omitted: number;
  global: { guidance: GuidanceMode; notes_chars: number };
  repo: { guidance: GuidanceMode | null; notes: string | null };
  effective_guidance: GuidanceMode;
  max_bytes: number;
}
