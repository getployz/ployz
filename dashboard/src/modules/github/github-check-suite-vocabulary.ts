export const GITHUB_CHECK_SUITE_ACTIONS = [
  "requested",
  "rerequested",
  "completed",
] as const;
export type GithubCheckSuiteAction =
  (typeof GITHUB_CHECK_SUITE_ACTIONS)[number];

export const GITHUB_CHECK_SUITE_STATUSES = [
  "queued",
  "in_progress",
  "completed",
  "pending",
  "waiting",
  "requested",
] as const;
export type GithubCheckSuiteStatus =
  (typeof GITHUB_CHECK_SUITE_STATUSES)[number];

export const GITHUB_CHECK_SUITE_CONCLUSIONS = [
  "action_required",
  "cancelled",
  "failure",
  "neutral",
  "success",
  "skipped",
  "stale",
  "timed_out",
  "startup_failure",
] as const;
export type GithubCheckSuiteConclusion =
  (typeof GITHUB_CHECK_SUITE_CONCLUSIONS)[number];
