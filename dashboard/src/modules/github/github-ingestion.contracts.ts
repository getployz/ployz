import { DateTime, Option, Schema, SchemaGetter } from "effect";

export const GITHUB_CHECK_SUITE_ACTIONS = [
  "requested",
  "rerequested",
  "completed",
] as const;

const GITHUB_CHECK_SUITE_STATUSES = [
  "queued",
  "in_progress",
  "completed",
  "pending",
  "waiting",
  "requested",
] as const;

const GITHUB_CHECK_SUITE_CONCLUSIONS = [
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

const GITHUB_COMPARE_STATUSES = [
  "ahead",
  "behind",
  "diverged",
  "identical",
] as const;

function containsAsciiControl(value: string): boolean {
  return Array.from(value).some((character) => {
    const code = character.charCodeAt(0);
    return code < 32 || code === 127;
  });
}

const forbiddenGitRefCharacters = new Set(["~", "^", ":", "?", "*", "[", "\\"]);

function isSafeGithubBranchRef(ref: string): boolean {
  const prefix = "refs/heads/";
  if (!ref.startsWith(prefix)) return false;
  const branch = ref.slice(prefix.length);
  if (
    branch.length === 0 ||
    branch.startsWith("/") ||
    branch.endsWith("/") ||
    branch.endsWith(".") ||
    branch.includes("//") ||
    branch.includes("..") ||
    branch.includes("@{") ||
    branch.includes(" ") ||
    containsAsciiControl(branch) ||
    Array.from(branch).some((character) =>
      forbiddenGitRefCharacters.has(character),
    )
  ) {
    return false;
  }
  return branch
    .split("/")
    .every(
      (component) =>
        component.length > 0 &&
        !component.startsWith(".") &&
        component !== "." &&
        component !== ".." &&
        !component.endsWith(".lock"),
    );
}

function isCanonicalRepositoryPath(value: string): boolean {
  if (
    value.length === 0 ||
    value.includes("\0") ||
    value.includes("\\") ||
    value.startsWith("/") ||
    /^[A-Za-z]:\//.test(value) ||
    value.endsWith("/") ||
    value.includes("//") ||
    containsAsciiControl(value)
  ) {
    return false;
  }
  return value
    .split("/")
    .every((segment) => segment !== "." && segment !== "..");
}

export const githubNonEmptyStringSchema = Schema.String.check(Schema.isMinLength(1));

/** A slash-separated GitHub name or path with each segment URL-encoded, for a request path. */
export const encodeGithubPath = (path: string) => path.split("/").map(encodeURIComponent).join("/");
const positiveSafeIntegerSchema = Schema.Finite.check(
  Schema.isInt(),
  Schema.isGreaterThan(0),
  Schema.isLessThanOrEqualTo(Number.MAX_SAFE_INTEGER),
);

export const githubIdSchema = positiveSafeIntegerSchema;
export const githubSafeBranchRefSchema = Schema.String.check(
  Schema.makeFilter(isSafeGithubBranchRef),
);

const githubExactShaEncodedSchema = Schema.String.check(
  Schema.isPattern(/^[0-9a-fA-F]{40}$/),
);
const githubExactShaTypeSchema = Schema.String.check(
  Schema.isPattern(/^[0-9a-f]{40}$/),
);
export const githubExactShaSchema = githubExactShaEncodedSchema.pipe(
  Schema.decodeTo(githubExactShaTypeSchema, {
    decode: SchemaGetter.transform((sha) => sha.toLowerCase()),
    encode: SchemaGetter.transform((sha) => sha),
  }),
);

export const githubBranchNameSchema = Schema.String.check(
  Schema.makeFilter((name) => isSafeGithubBranchRef(`refs/heads/${name}`)),
);

export const githubRepositoryPathSchema = Schema.String.check(
  Schema.makeFilter(isCanonicalRepositoryPath),
);

const githubChangedPathsTypeSchema = Schema.Array(githubRepositoryPathSchema);
export const githubChangedPathsSchema = Schema.Array(
  githubRepositoryPathSchema,
).pipe(
  Schema.decodeTo(githubChangedPathsTypeSchema, {
    decode: SchemaGetter.transform((paths) =>
      Array.from(new Set(paths)).sort((left, right) => left.localeCompare(right)),
    ),
    encode: SchemaGetter.transform((paths) => paths),
  }),
);

export const githubCheckSuiteActionSchema = Schema.Literals(
  GITHUB_CHECK_SUITE_ACTIONS,
);
export const githubCheckSuiteStatusSchema = Schema.Literals(
  GITHUB_CHECK_SUITE_STATUSES,
);
export const githubCheckSuiteConclusionSchema = Schema.Literals(
  GITHUB_CHECK_SUITE_CONCLUSIONS,
);
export const githubCompareStatusSchema = Schema.Literals(
  GITHUB_COMPARE_STATUSES,
);

const githubTimestampEncodedSchema = Schema.String.check(
  Schema.isPattern(
    /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/,
  ),
  Schema.makeFilter((value) => Option.isSome(DateTime.make(value))),
);
export const githubTimestampSchema = githubTimestampEncodedSchema.pipe(
  Schema.decodeTo(Schema.String, {
    decode: SchemaGetter.transform((value) => new Date(value).toISOString()),
    encode: SchemaGetter.transform((value) => value),
  }),
);

export const githubRepositoryFullNameSchema = Schema.String.check(
  Schema.isPattern(
    /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9_.-]+$/,
  ),
  Schema.makeFilter((value) => {
    const repository = value.split("/")[1];
    return repository !== "." && repository !== "..";
  }),
);

export const githubPushWebhookSchema = Schema.Struct({
  kind: Schema.Literal("push"),
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  ref: githubSafeBranchRefSchema,
  branch: githubNonEmptyStringSchema,
  beforeSha: githubExactShaSchema,
  afterSha: githubExactShaSchema,
  created: Schema.Boolean,
  deleted: Schema.Boolean,
  forced: Schema.Boolean,
});

export const githubCheckSuiteWebhookSchema = Schema.Struct({
  kind: Schema.Literal("check_suite"),
  action: githubCheckSuiteActionSchema,
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  checkSuiteId: githubIdSchema,
  headSha: githubExactShaSchema,
  status: githubCheckSuiteStatusSchema,
  conclusion: Schema.NullOr(githubCheckSuiteConclusionSchema),
  sourceUpdatedAt: githubTimestampSchema,
});

// The pull request actions Cloud records; GitHub's others are answered and dropped.
export const GITHUB_PULL_REQUEST_ACTIONS = [
  "opened",
  "reopened",
  "synchronize",
  "closed",
  "edited",
] as const;
export const githubPullRequestActionSchema = Schema.Literals(
  GITHUB_PULL_REQUEST_ACTIONS,
);

export const githubPullRequestWebhookSchema = Schema.Struct({
  kind: Schema.Literal("pull_request"),
  action: githubPullRequestActionSchema,
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  number: githubIdSchema,
  // Null when the pull request's fork was deleted.
  headRepositoryId: Schema.NullOr(githubIdSchema),
  headSha: githubExactShaSchema,
});

/** The identity a delivery's per-branch, per-check-suite or per-pull-request event is ordered by. */
export const githubPushBranchKey = (push: { installationId: number; repositoryId: number; ref: string }) =>
  `${push.installationId}:${push.repositoryId}:${push.ref}`;
export const githubCheckSuiteKey = (suite: { installationId: number; repositoryId: number; checkSuiteId: number }) =>
  `${suite.installationId}:${suite.repositoryId}:${suite.checkSuiteId}`;
export const githubPullRequestKey = (pullRequest: { installationId: number; repositoryId: number; number: number }) =>
  `${pullRequest.installationId}:${pullRequest.repositoryId}:${pullRequest.number}`;

export const githubPushReceivedEventInputSchema = Schema.Struct({
  ...githubPushWebhookSchema.fields,
  deliveryId: githubNonEmptyStringSchema,
});
export const githubPushReceivedEventDataSchema = Schema.Struct({
  ...githubPushReceivedEventInputSchema.fields,
  branchKey: githubNonEmptyStringSchema,
}).check(
  Schema.makeFilter(
    (value) =>
      value.branchKey === githubPushBranchKey(value) &&
      value.ref === `refs/heads/${value.branch}`,
    {
      message: "GitHub push event authority key does not match its identity.",
    },
  ),
);

export const githubCheckSuiteReceivedEventInputSchema = Schema.Struct({
  ...githubCheckSuiteWebhookSchema.fields,
  deliveryId: githubNonEmptyStringSchema,
});
export const githubCheckSuiteReceivedEventDataSchema = Schema.Struct({
  ...githubCheckSuiteReceivedEventInputSchema.fields,
  checkSuiteKey: githubNonEmptyStringSchema,
}).check(
  Schema.makeFilter(
    (value) =>
      value.checkSuiteKey === githubCheckSuiteKey(value),
    {
      message: "GitHub check-suite authority key does not match its identity.",
    },
  ),
);

export const githubPullRequestReceivedEventInputSchema = Schema.Struct({
  ...githubPullRequestWebhookSchema.fields,
  deliveryId: githubNonEmptyStringSchema,
});
export const githubPullRequestReceivedEventDataSchema = Schema.Struct({
  ...githubPullRequestReceivedEventInputSchema.fields,
  pullRequestKey: githubNonEmptyStringSchema,
}).check(
  Schema.makeFilter(
    (value) =>
      value.pullRequestKey === githubPullRequestKey(value),
    {
      message: "GitHub pull request authority key does not match its identity.",
    },
  ),
);

export const githubResolvedRepositorySchema = Schema.Struct({
  id: githubIdSchema,
  fullName: githubRepositoryFullNameSchema,
});
export const isValidGithubId: <Input>(input: Input) => input is Input & number =
  Schema.is(githubIdSchema);

export const isValidGithubBranchRef: <Input>(input: Input) => input is Input & string =
  Schema.is(githubSafeBranchRefSchema);

// `githubExactShaSchema` is a transform: the encoded side accepts uppercase
// hex and decoding lowercases it. `Schema.is` checks the decoded side only, so
// this guard decodes to keep accepting an uppercase SHA.
export function isValidGithubExactSha<Input>(
  input: Input,
): input is Input & string {
  return Option.isSome(Schema.decodeUnknownOption(githubExactShaSchema)(input));
}

export type GithubPushWebhook = typeof githubPushWebhookSchema.Type;
export type GithubCheckSuiteWebhook = typeof githubCheckSuiteWebhookSchema.Type;
export type GithubPullRequestAction = typeof githubPullRequestActionSchema.Type;
export type GithubPullRequestWebhook = typeof githubPullRequestWebhookSchema.Type;
export type GithubPullRequestReceivedEventInput =
  typeof githubPullRequestReceivedEventInputSchema.Type;
export type GithubPullRequestReceivedEventData =
  typeof githubPullRequestReceivedEventDataSchema.Type;
export type GithubPushReceivedEventInput =
  typeof githubPushReceivedEventInputSchema.Type;
export type GithubPushReceivedEventData =
  typeof githubPushReceivedEventDataSchema.Type;
export type GithubCheckSuiteReceivedEventInput =
  typeof githubCheckSuiteReceivedEventInputSchema.Type;
export type GithubCheckSuiteReceivedEventData =
  typeof githubCheckSuiteReceivedEventDataSchema.Type;
export type GithubResolvedRepository =
  typeof githubResolvedRepositorySchema.Type;
