import { minimatch } from "minimatch";
import { Data, Result, Schema } from "effect";
import {
  GITHUB_CHECK_SUITE_ACTIONS,
  githubChangedPathsSchema,
  githubCheckSuiteActionSchema,
  githubCheckSuiteConclusionSchema,
  githubCheckSuiteStatusSchema,
  githubExactShaSchema,
  githubIdSchema,
  githubSafeBranchRefSchema,
  githubTimestampSchema,
  githubWatchPatternSchema,
  type GithubCheckSuiteWebhook,
  type GithubPushWebhook,
} from "#/modules/github/github-ingestion.contracts";
import { asRecord, asString } from "#/lib/json";

const githubIdentityFields = {
  installation: Schema.Struct({ id: githubIdSchema }),
  repository: Schema.Struct({ id: githubIdSchema }),
};
const githubPushPayloadSchema = Schema.Struct({
  ...githubIdentityFields,
  ref: githubSafeBranchRefSchema,
  before: githubExactShaSchema,
  after: githubExactShaSchema,
  created: Schema.Boolean,
  deleted: Schema.Boolean,
  forced: Schema.Boolean,
});
const githubCheckSuitePayloadSchema = Schema.Struct({
  ...githubIdentityFields,
  action: githubCheckSuiteActionSchema,
  check_suite: Schema.Struct({
    id: githubIdSchema,
    head_sha: githubExactShaSchema,
    status: githubCheckSuiteStatusSchema,
    conclusion: Schema.NullOr(githubCheckSuiteConclusionSchema),
    updated_at: githubTimestampSchema,
  }),
});
const nonEmptyStringSchema = Schema.String.check(Schema.isMinLength(1));
const githubInstallationPayloadSchema = Schema.Struct({
  action: Schema.Literals([
    "created",
    "deleted",
    "suspend",
    "unsuspend",
    "new_permissions_accepted",
  ]),
  installation: Schema.Struct({
    id: githubIdSchema,
    account: Schema.Struct({
      login: nonEmptyStringSchema,
      type: nonEmptyStringSchema,
      avatar_url: nonEmptyStringSchema,
    }),
  }),
  sender: Schema.Struct({
    id: githubIdSchema,
    login: nonEmptyStringSchema,
  }),
});
const githubInstallationRepositorySchema = Schema.Struct({
  id: githubIdSchema,
  name: nonEmptyStringSchema,
  full_name: nonEmptyStringSchema,
  private: Schema.Boolean,
  default_branch: nonEmptyStringSchema,
  html_url: nonEmptyStringSchema,
  updated_at: githubTimestampSchema,
});
const githubInstallationRepositoriesPayloadSchema = Schema.Struct({
  action: Schema.Literals(["added", "removed"]),
  installation: Schema.Struct({ id: githubIdSchema }),
  repositories_added: Schema.Array(githubInstallationRepositorySchema),
  repositories_removed: Schema.Array(githubInstallationRepositorySchema),
  sender: Schema.Struct({
    id: githubIdSchema,
    login: nonEmptyStringSchema,
  }),
});

export const githubInstallationWebhookEventDataSchema = Schema.Struct({
  deliveryId: nonEmptyStringSchema,
  ...githubInstallationPayloadSchema.fields,
});

export const githubInstallationRepositoriesWebhookEventDataSchema =
  Schema.Struct({
    deliveryId: nonEmptyStringSchema,
    ...githubInstallationRepositoriesPayloadSchema.fields,
  });

export type GithubInstallationWebhook =
  typeof githubInstallationPayloadSchema.Type;
export type GithubInstallationRepositoriesWebhook =
  typeof githubInstallationRepositoriesPayloadSchema.Type;

export class GithubWebhookContractError extends Data.TaggedError(
  "GithubWebhookContractError",
)<{
  readonly code: "malformed_payload" | "unsupported_action";
  readonly message: string;
}> {}

export class GithubPathContractError extends Data.TaggedError(
  "GithubPathContractError",
)<{
  readonly code: "invalid_changed_path" | "invalid_watch_pattern";
  readonly message: string;
}> {}

export type {
  GithubCheckSuiteWebhook,
  GithubPushWebhook,
} from "#/modules/github/github-ingestion.contracts";

export function decodeGithubPushPayload<Input>(
  payload: Input,
): Result.Result<GithubPushWebhook, GithubWebhookContractError> {
  const decoded = Schema.decodeUnknownResult(githubPushPayloadSchema)(payload);
  if (Result.isFailure(decoded)) {
    return Result.fail(
      new GithubWebhookContractError({
        code: "malformed_payload",
        message: "Malformed GitHub push payload.",
      }),
    );
  }

  const { installation, repository, ref, before, after, ...flags } =
    decoded.success;
  return Result.succeed({
    kind: "push",
    installationId: installation.id,
    repositoryId: repository.id,
    ref,
    branch: ref.slice("refs/heads/".length),
    beforeSha: before,
    afterSha: after,
    ...flags,
  });
}

const githubCheckSuiteActions = new Set<string>(GITHUB_CHECK_SUITE_ACTIONS);

export function decodeGithubCheckSuitePayload<Input>(
  payload: Input,
): Result.Result<GithubCheckSuiteWebhook, GithubWebhookContractError> {
  const decoded =
    Schema.decodeUnknownResult(githubCheckSuitePayloadSchema)(payload);
  if (Result.isFailure(decoded)) {
    const action = asString(asRecord(payload)?.["action"]);
    const isUnsupportedAction =
      action !== null && !githubCheckSuiteActions.has(action);
    return Result.fail(
      new GithubWebhookContractError({
        code: isUnsupportedAction
          ? "unsupported_action"
          : "malformed_payload",
        message: isUnsupportedAction
          ? "Unsupported GitHub check-suite action."
          : "Malformed GitHub check-suite payload.",
      }),
    );
  }

  const { installation, repository, check_suite: checkSuite, action } =
    decoded.success;
  return Result.succeed({
    kind: "check_suite",
    action,
    installationId: installation.id,
    repositoryId: repository.id,
    checkSuiteId: checkSuite.id,
    headSha: checkSuite.head_sha,
    status: checkSuite.status,
    conclusion: checkSuite.conclusion,
    sourceUpdatedAt: checkSuite.updated_at,
  });
}

export function decodeGithubInstallationPayload<Input>(
  payload: Input,
): Result.Result<GithubInstallationWebhook, GithubWebhookContractError> {
  const decoded =
    Schema.decodeUnknownResult(githubInstallationPayloadSchema)(payload);
  return Result.isSuccess(decoded)
    ? Result.succeed(decoded.success)
    : Result.fail(
        new GithubWebhookContractError({
          code: "malformed_payload",
          message: "Malformed GitHub installation payload.",
        }),
      );
}

export function decodeGithubInstallationRepositoriesPayload<Input>(
  payload: Input,
): Result.Result<
  GithubInstallationRepositoriesWebhook,
  GithubWebhookContractError
> {
  const decoded = Schema.decodeUnknownResult(
    githubInstallationRepositoriesPayloadSchema,
  )(payload);
  return Result.isSuccess(decoded)
    ? Result.succeed(decoded.success)
    : Result.fail(
        new GithubWebhookContractError({
          code: "malformed_payload",
          message: "Malformed GitHub installation repositories payload.",
        }),
      );
}

export function canonicalizeGithubChangedPaths<Input>(
  paths: Input,
): Result.Result<readonly string[], GithubPathContractError> {
  const parsed = Schema.decodeUnknownResult(githubChangedPathsSchema)(paths);
  return Result.isSuccess(parsed)
    ? Result.succeed(parsed.success)
    : Result.fail(
        new GithubPathContractError({
          code: "invalid_changed_path",
          message: "Changed path is not repository-relative and canonical.",
        }),
      );
}

type CompiledWatchPattern = {
  readonly negated: boolean;
  readonly pattern: string;
  readonly matchBase: boolean;
};

function hasBalancedGlobDelimiters(value: string): boolean {
  let inCharacterClass = false;
  let braceDepth = 0;
  let extglobDepth = 0;
  const characters = Array.from(value);
  for (const [index, character] of characters.entries()) {
    if (inCharacterClass) {
      if (character === "]") inCharacterClass = false;
      continue;
    }
    if (character === "[") {
      inCharacterClass = true;
    } else if (character === "{") {
      braceDepth += 1;
    } else if (character === "}") {
      if (braceDepth === 0) return false;
      braceDepth -= 1;
    } else if (
      character === "(" &&
      ["?", "*", "+", "@", "!"].includes(characters[index - 1] ?? "")
    ) {
      extglobDepth += 1;
    } else if (character === ")" && extglobDepth > 0) {
      extglobDepth -= 1;
    }
  }
  return !inCharacterClass && braceDepth === 0 && extglobDepth === 0;
}

function invalidWatchPattern(): Result.Result<never, GithubPathContractError> {
  return Result.fail(
    new GithubPathContractError({
      code: "invalid_watch_pattern",
      message: "Watch path pattern is malformed or unsafe.",
    }),
  );
}

function compileWatchPattern<Input>(
  value: Input,
): Result.Result<CompiledWatchPattern, GithubPathContractError> {
  const parsed = Schema.decodeUnknownResult(githubWatchPatternSchema)(value);
  if (Result.isFailure(parsed)) return invalidWatchPattern();

  const negated = parsed.success.startsWith("!");
  let pattern = negated ? parsed.success.slice(1) : parsed.success;
  if (
    pattern.length === 0 ||
    pattern.startsWith("!") ||
    pattern.startsWith("//")
  ) {
    return invalidWatchPattern();
  }

  const rootAnchored = pattern.startsWith("/");
  if (rootAnchored) pattern = pattern.slice(1);
  const directoryPattern = pattern.endsWith("/");
  if (directoryPattern) pattern = pattern.slice(0, -1);
  if (
    pattern.length === 0 ||
    pattern.includes("//") ||
    /^[A-Za-z]:\//.test(pattern) ||
    !hasBalancedGlobDelimiters(pattern) ||
    pattern.split("/").some((segment) => segment === "." || segment === "..")
  ) {
    return invalidWatchPattern();
  }

  const containsSlash = pattern.includes("/");
  if (directoryPattern) {
    pattern = rootAnchored || containsSlash ? `${pattern}/**` : `**/${pattern}/**`;
  }
  return Result.succeed({
    negated,
    pattern,
    matchBase: !rootAnchored && !directoryPattern && !containsSlash,
  });
}

export function matchGithubWatchPaths(input: {
  readonly changedPaths: unknown;
  readonly watchPaths: readonly unknown[];
}): Result.Result<boolean, GithubPathContractError> {
  const changedPaths = canonicalizeGithubChangedPaths(input.changedPaths);
  if (Result.isFailure(changedPaths)) return Result.fail(changedPaths.failure);
  if (input.watchPaths.length === 0) {
    return Result.succeed(changedPaths.success.length > 0);
  }

          const compiled = Result.all(input.watchPaths.map(compileWatchPattern));
  if (Result.isFailure(compiled)) return Result.fail(compiled.failure);
  const patterns = compiled.success;

  for (const changedPath of changedPaths.success) {
    let selected = false;
    for (const pattern of patterns) {
      if (
        minimatch(changedPath, pattern.pattern, {
          dot: true,
          matchBase: pattern.matchBase,
          nocomment: true,
          nonegate: true,
        })
      ) {
        selected = !pattern.negated;
      }
    }
    if (selected) return Result.succeed(true);
  }

  return Result.succeed(false);
}
