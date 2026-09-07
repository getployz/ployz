import { DateTime, Option, Schema, SchemaGetter } from "effect";
import {
  GITHUB_CHECK_SUITE_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
  type GithubCheckSuiteAction,
  type GithubCheckSuiteConclusion,
  type GithubCheckSuiteStatus,
} from "#/modules/github/github-check-suite-vocabulary";

export {
  GITHUB_CHECK_SUITE_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
};
export type {
  GithubCheckSuiteAction,
  GithubCheckSuiteConclusion,
  GithubCheckSuiteStatus,
};

export const GITHUB_COMPARE_STATUSES = [
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

const nonEmptyStringSchema = Schema.String.check(Schema.isMinLength(1));
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

export const githubRepositoryPathSchema = Schema.String.check(
  Schema.makeFilter(isCanonicalRepositoryPath),
);
export const githubWatchPatternSchema = Schema.String.check(
  Schema.isMinLength(1),
  Schema.makeFilter(
    (value) =>
      !value.includes("\0") &&
      !value.includes("\\") &&
      !containsAsciiControl(value),
  ),
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

export const githubServiceCandidateSchema = Schema.Struct({
  environmentId: nonEmptyStringSchema,
  serviceId: nonEmptyStringSchema,
  watchPaths: Schema.Array(Schema.String),
});

export const githubPushWebhookSchema = Schema.Struct({
  kind: Schema.Literal("push"),
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  ref: githubSafeBranchRefSchema,
  branch: nonEmptyStringSchema,
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

export const githubPushReceivedEventInputSchema = Schema.Struct({
  ...githubPushWebhookSchema.fields,
  deliveryId: nonEmptyStringSchema,
});
export const githubPushReceivedEventDataSchema = Schema.Struct({
  ...githubPushReceivedEventInputSchema.fields,
  branchKey: nonEmptyStringSchema,
}).check(
  Schema.makeFilter(
    (value) =>
      value.branchKey ===
        `${value.installationId}:${value.repositoryId}:${value.ref}` &&
      value.ref === `refs/heads/${value.branch}`,
    {
      message: "GitHub push event authority key does not match its identity.",
    },
  ),
);

export const githubCheckSuiteReceivedEventInputSchema = Schema.Struct({
  ...githubCheckSuiteWebhookSchema.fields,
  deliveryId: nonEmptyStringSchema,
});
export const githubCheckSuiteReceivedEventDataSchema = Schema.Struct({
  ...githubCheckSuiteReceivedEventInputSchema.fields,
  checkSuiteKey: nonEmptyStringSchema,
}).check(
  Schema.makeFilter(
    (value) =>
      value.checkSuiteKey ===
      `${value.installationId}:${value.repositoryId}:${value.checkSuiteId}`,
    {
      message: "GitHub check-suite authority key does not match its identity.",
    },
  ),
);

export const githubEnvironmentTriggerSelectionSchema = Schema.Union([
  Schema.Struct({
    mode: Schema.Literal("paths"),
    reason: Schema.Literal("changed_paths"),
  }),
  Schema.Struct({
    mode: Schema.Literal("all_services"),
    reason: Schema.Literals([
      "first_observation",
      "force_rebaseline",
      "non_ancestor_rebaseline",
      "changed_paths_incomplete_rebaseline",
    ]),
  }),
]);

export const githubEnvironmentTriggerPersistedEventDataSchema = Schema.Struct({
  triggerId: nonEmptyStringSchema,
  triggerRevision: positiveSafeIntegerSchema,
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  ref: githubSafeBranchRefSchema,
  headSha: githubExactShaSchema,
  environmentId: nonEmptyStringSchema,
  serviceIds: Schema.Array(nonEmptyStringSchema),
  selection: githubEnvironmentTriggerSelectionSchema,
  sourceDeliveryId: nonEmptyStringSchema,
  sourceReceiptSequence: positiveSafeIntegerSchema,
});

export const githubCheckSuiteTransitionEventDataSchema = Schema.Struct({
  installationId: githubIdSchema,
  repositoryId: githubIdSchema,
  headSha: githubExactShaSchema,
  checkSuiteId: githubIdSchema,
  status: githubCheckSuiteStatusSchema,
  conclusion: Schema.NullOr(githubCheckSuiteConclusionSchema),
  sourceUpdatedAt: githubTimestampSchema,
  transitionRevision: positiveSafeIntegerSchema,
  sourceDeliveryId: nonEmptyStringSchema,
  sourceReceiptSequence: positiveSafeIntegerSchema,
});

export const githubResolvedRepositorySchema = Schema.Struct({
  id: githubIdSchema,
  fullName: githubRepositoryFullNameSchema,
});
export const githubBranchHeadObservationSchema = Schema.Union([
  Schema.Struct({
    state: Schema.Literal("present"),
    ref: githubSafeBranchRefSchema,
    headSha: githubExactShaSchema,
  }),
  Schema.Struct({
    state: Schema.Literal("absent"),
    ref: githubSafeBranchRefSchema,
  }),
]);
export const githubCompareObservationSchema = Schema.Struct({
  status: githubCompareStatusSchema,
  baseSha: githubExactShaSchema,
  headSha: githubExactShaSchema,
  changedPaths: githubChangedPathsSchema,
  pathsComplete: Schema.Boolean,
});
export const githubCheckSuiteObservationSchema = Schema.Struct({
  checkSuiteId: githubIdSchema,
  headSha: githubExactShaSchema,
  status: githubCheckSuiteStatusSchema,
  conclusion: Schema.NullOr(githubCheckSuiteConclusionSchema),
  updatedAt: githubTimestampSchema,
});
export const githubBranchCursorSchema = Schema.Union([
  Schema.Struct({
    state: Schema.Literal("active"),
    headSha: githubExactShaSchema,
    evaluationReason: Schema.Literals([
      "first_observation",
      "changed_paths",
      "rebaseline_all_services",
    ]),
    evaluationRevision: positiveSafeIntegerSchema,
    lastDeliveryId: nonEmptyStringSchema,
    lastReceiptSequence: positiveSafeIntegerSchema,
  }),
  Schema.Struct({
    state: Schema.Literal("deleted"),
    evaluationReason: Schema.Literal("branch_deleted"),
    evaluationRevision: positiveSafeIntegerSchema,
    lastDeliveryId: nonEmptyStringSchema,
    lastReceiptSequence: positiveSafeIntegerSchema,
  }),
]);
export const githubEnvironmentTriggerInputSchema = Schema.Struct({
  environmentId: nonEmptyStringSchema,
  serviceIds: Schema.Array(nonEmptyStringSchema).check(Schema.isMinLength(1)),
  selection: githubEnvironmentTriggerSelectionSchema,
});

export function isValidGithubId<Input>(input: Input): input is Input & number {
  return Option.isSome(Schema.decodeUnknownOption(githubIdSchema)(input));
}

export function isValidGithubBranchRef<Input>(
  input: Input,
): input is Input & string {
  return Option.isSome(
    Schema.decodeUnknownOption(githubSafeBranchRefSchema)(input),
  );
}

export function isValidGithubExactSha<Input>(
  input: Input,
): input is Input & string {
  return Option.isSome(Schema.decodeUnknownOption(githubExactShaSchema)(input));
}

export function isValidGithubServiceCandidate<Input>(
  input: Input,
): input is Input & typeof githubServiceCandidateSchema.Type {
  return Option.isSome(
    Schema.decodeUnknownOption(githubServiceCandidateSchema)(input, {
      onExcessProperty: "error",
    }),
  );
}

export function isValidGithubEnvironmentTriggerSelection<Input>(
  input: Input,
): input is Input & typeof githubEnvironmentTriggerSelectionSchema.Type {
  return Option.isSome(
    Schema.decodeUnknownOption(githubEnvironmentTriggerSelectionSchema)(
      input,
      { onExcessProperty: "error" },
    ),
  );
}

export type GithubId = typeof githubIdSchema.Type;
export type GithubCompareStatus = typeof githubCompareStatusSchema.Type;
export type GithubServiceCandidate = typeof githubServiceCandidateSchema.Type;
export type GithubPushWebhook = typeof githubPushWebhookSchema.Type;
export type GithubCheckSuiteWebhook = typeof githubCheckSuiteWebhookSchema.Type;
export type GithubPushReceivedEventInput =
  typeof githubPushReceivedEventInputSchema.Type;
export type GithubPushReceivedEventData =
  typeof githubPushReceivedEventDataSchema.Type;
export type GithubCheckSuiteReceivedEventInput =
  typeof githubCheckSuiteReceivedEventInputSchema.Type;
export type GithubCheckSuiteReceivedEventData =
  typeof githubCheckSuiteReceivedEventDataSchema.Type;
export type GithubEnvironmentTriggerPersistedEventData =
  typeof githubEnvironmentTriggerPersistedEventDataSchema.Type;
export type GithubCheckSuiteTransitionEventData =
  typeof githubCheckSuiteTransitionEventDataSchema.Type;
export type GithubResolvedRepository =
  typeof githubResolvedRepositorySchema.Type;
export type GithubBranchHeadObservation =
  typeof githubBranchHeadObservationSchema.Type;
export type GithubCompareObservation =
  typeof githubCompareObservationSchema.Type;
export type GithubCheckSuiteObservation =
  typeof githubCheckSuiteObservationSchema.Type;
export type GithubBranchCursor = typeof githubBranchCursorSchema.Type;
export type GithubEnvironmentTriggerSelection =
  typeof githubEnvironmentTriggerSelectionSchema.Type;
export type GithubEnvironmentTriggerInput =
  typeof githubEnvironmentTriggerInputSchema.Type;
