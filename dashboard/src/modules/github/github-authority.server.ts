import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import { AwsClient } from "aws4fetch";
import { Cache, Context, Duration, Effect, Exit, Layer, Redacted, Schema } from "effect";
import { GITHUB_BUILD_WORKFLOW, GITHUB_BUILD_WORKFLOW_FILE } from "#/modules/github/github-build-workflow";
import { githubIdSchema, githubTimestampSchema } from "#/modules/github/github-ingestion.contracts";
import { GithubObservationError, githubResponseError, type GithubObservationOperation } from "#/modules/github/github-observation.api";
import { AppConfig, type GithubMinterAuth } from "#/server/config.server";

/**
 * Everything Cloud does in GitHub with the App's own authority: the App JWT, installation tokens, the build workflow's
 * runs, the Ployz check and the connect code. Nothing else in Cloud reads the App's key or client secret, mints a
 * token, or calls an Actions path, so one layer decides who holds that authority. The local layer holds it in Cloud,
 * as a Self-hosted Cloud does. The minter layer asks the minter, which holds the key outside Cloud and grants Cloud
 * only read tokens and the narrow operations below.
 */
export const GITHUB_API_VERSION = "2022-11-28";

export type GithubReadPermission = "contents" | "metadata" | "pull_requests" | "checks";
export type GithubReadPermissions = Partial<Record<GithubReadPermission, "read">>;

/** The most a token Cloud holds may read. A token request names a subset, or gets all of it. */
export const GITHUB_READ_SET = { contents: "read", metadata: "read", pull_requests: "read", checks: "read" } as const satisfies GithubReadPermissions;

const BUILD_WORKFLOW_PATH = `.github/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`;

/** What each operation's own token may do, for that one repository. */
const OWN_PERMISSIONS = {
  workflow: { contents: "read", actions: "read" },
  runs: { actions: "read" },
  dispatch: { contents: "read", actions: "write" },
  act: { actions: "write" },
  check: { checks: "write" },
} as const satisfies Record<string, Readonly<Record<string, "read" | "write">>>;
type OwnNeed = keyof typeof OWN_PERMISSIONS;

/** An installation token lives an hour; an operation's own token is kept for less, so none expires mid-call. */
const OWN_TOKEN_TTL = Duration.minutes(50);

/** GitHub refuses the whole mint when it names a permission the installation never accepted. */
const unaccepted = (error: GithubObservationError) => error.operation === "installation_token" && error.status === 422;

/**
 * A read token with `scope`'s permissions, or the whole read set. An installation that never accepted, say, Checks
 * still reads its code: on GitHub's refusal, the token asks only for what the installation accepted.
 */
function readSetToken(
  installationId: number,
  scope: GithubReadScope | undefined,
  installation: GithubAuthorityService["installation"],
  mintWith: (permissions: GithubReadPermissions) => Effect.Effect<string, GithubObservationError>,
): Effect.Effect<string, GithubObservationError> {
  if (scope?.permissions !== undefined) return mintWith(scope.permissions);
  return mintWith(GITHUB_READ_SET).pipe(Effect.catchIf(unaccepted, () => installation(installationId).pipe(
    Effect.flatMap(({ permissions }) => mintWith(Object.fromEntries(Object.keys(GITHUB_READ_SET)
      .filter((name) => permissions[name] === "read" || permissions[name] === "write")
      .map((name) => [name, "read"])))),
  )));
}

/** SHA-256 of a workflow file after CRLF becomes LF, as the minter compares it. */
export function githubWorkflowDigest(bytes: Uint8Array): string {
  const text = Buffer.from(bytes).toString("latin1").replaceAll("\r\n", "\n");
  return crypto.createHash("sha256").update(Buffer.from(text, "latin1")).digest("hex");
}

/** The `ployz-build.yml` files Ployz dispatches: the ones the dashboard offers. */
export const GITHUB_BUILD_WORKFLOW_DIGESTS: ReadonlySet<string> = new Set([githubWorkflowDigest(Buffer.from(GITHUB_BUILD_WORKFLOW))]);

export type GithubRepositoryRef = { readonly installationId: number; readonly repositoryId: number };
export type GithubBuildRunRef = GithubRepositoryRef & { readonly runId: number };

/** Which repositories and read permissions a read token covers. Left out, every repository and the whole read set. */
export type GithubReadScope = { readonly repositoryIds?: ReadonlyArray<number>; readonly permissions?: GithubReadPermissions };

/** A build workflow run's metadata. Never its jobs, logs or artifacts. */
export type GithubBuildRun = {
  readonly id: number;
  readonly url: string;
  readonly headSha: string;
  readonly event: string;
  readonly status: string | null;
  readonly conclusion: string | null;
};

/** The build workflow on the repository's default branch: `state` is null when GitHub lists no such workflow. */
export type GithubBuildWorkflowFile = {
  readonly fullName: string;
  readonly defaultBranch: string;
  readonly state: string | null;
  readonly canonical: boolean;
};

export type GithubBuildDispatch = GithubRepositoryRef & {
  readonly branch: string;
  /** `cloud` is not one: whoever holds the authority fills it with this Cloud's origin. */
  readonly inputs: { readonly build: string; readonly runner?: string; readonly commit?: string };
};

export type GithubDispatchedBuild = { readonly runId: number; readonly runUrl: string; readonly headSha: string; readonly workflowRef: string };

/** A commit a person approved or reviewed under a rule, built by the rule's workflow on the rule's branch. */
export type GithubApprovedDispatch = GithubRepositoryRef & { readonly branch: string; readonly commit: string };

/** The Ployz check on a commit, completed. Two checks of one name on one commit stay apart by `externalId`. */
export type GithubCheck = GithubRepositoryRef & {
  readonly headSha: string;
  readonly name: string;
  readonly externalId: string;
  readonly conclusion: "success" | "neutral";
  readonly detailsUrl: string;
  readonly title: string;
  readonly summary: string;
};

/** Who authorized the App, and the installations they reach. Their user token never leaves the authority. */
export type GithubConnectedUser = {
  readonly login: string;
  readonly installations: ReadonlyArray<{
    readonly id: number;
    readonly account: { readonly login: string; readonly type: string; readonly avatarUrl: string | null };
    readonly suspendedAt: string | null;
    /**
     * Owns the account: its admin for an Organization, the person for a User. Reaching it is not owning it. Null when
     * GitHub wouldn't say, such as an Organization whose SSO the person hasn't signed in to.
     */
    readonly administers: boolean | null;
  }>;
};

type Failure = GithubObservationError;

export interface GithubAuthorityService {
  /** A token that reads `permissions` (the whole read set by default), on `repositoryIds` or every repository. */
  readonly readToken: (
    installationId: number,
    scope?: GithubReadScope,
  ) => Effect.Effect<string, Failure>;
  readonly installation: (installationId: number) => Effect.Effect<{ url: string; permissions: Readonly<Record<string, string>> }, Failure>;
  readonly buildWorkflow: (input: GithubRepositoryRef) => Effect.Effect<GithubBuildWorkflowFile, Failure>;
  readonly buildRuns: (input: GithubRepositoryRef & { readonly headSha: string; readonly event: "push" }) => Effect.Effect<ReadonlyArray<GithubBuildRun>, Failure>;
  readonly buildRun: (input: GithubBuildRunRef) => Effect.Effect<GithubBuildRun, Failure>;
  /** Refused unless the branch head's `ployz-build.yml` is one Ployz offers. */
  readonly dispatchBuild: (input: GithubBuildDispatch) => Effect.Effect<GithubDispatchedBuild, Failure>;
  /**
   * Dispatch the rule's `ployz-build.yml` on its branch with only `commit` as input. The file is the rule's own
   * template, which names its Cloud and builder, so no digest is checked: the grant Machine checks the commit against
   * what the person signed. The minter has no such route: it is refused there.
   */
  readonly dispatchApprovedBuild: (input: GithubApprovedDispatch) => Effect.Effect<{ readonly runId: number; readonly runUrl: string }, Failure>;
  /** A run that already finished can't be cancelled; that's fine. */
  readonly cancelBuild: (input: GithubBuildRunRef) => Effect.Effect<void, Failure>;
  /** Run a completed build run again, as the push that started it. The minter has no such route: it is refused there. */
  readonly rerunBuild: (input: GithubBuildRunRef) => Effect.Effect<void, Failure>;
  readonly postCheck: (input: GithubCheck) => Effect.Effect<{ checkRunId: number }, Failure>;
  readonly connect: (code: string) => Effect.Effect<GithubConnectedUser, Failure>;
}

export class GithubAuthority extends Context.Service<GithubAuthority, GithubAuthorityService>()("ployz/GithubAuthority") {}

const failure = (args: {
  code: GithubObservationError["code"]; operation: GithubObservationOperation; status?: number; retryAfterSeconds?: number; retriable: boolean; reason?: string;
}) => {
  const { reason, ...fields } = args;
  return new GithubObservationError({
    ...fields,
    message: reason === undefined ? `GitHub ${args.operation} failed (${args.code}).` : `GitHub ${args.operation} was refused: ${reason}`,
  });
};

const refused = (operation: GithubObservationOperation, reason: string) => failure({ code: "refused", operation, retriable: false, reason });

const invalidResponse = (operation: GithubObservationOperation) => failure({ code: "invalid_response", operation, retriable: false });

/** A JSON answer as `schema` reads it; anything else is an invalid response. */
const decodeAs = <S extends Schema.ConstraintDecoder<unknown>>(schema: S, text: string, operation: GithubObservationOperation) =>
  Effect.try({ try: () => JSON.parse(text), catch: () => invalidResponse(operation) }).pipe(
    Effect.flatMap((value) => Schema.decodeUnknownEffect(schema)(value).pipe(Effect.mapError(() => invalidResponse(operation)))),
  );

export function createGithubAppJwt(input: { readonly appId: string; readonly privateKey: string }): string {
  const privateKey = input.privateKey.replace(/\\n/g, "\n");
  const now = Math.floor(Date.now() / 1000);
  const header = Buffer.from(JSON.stringify({ alg: "RS256", typ: "JWT" })).toString("base64url");
  const payload = Buffer.from(JSON.stringify({ iss: input.appId, iat: now - 60, exp: now + 600 })).toString("base64url");
  const signature = crypto
    .createSign("RSA-SHA256")
    .update(`${header}.${payload}`)
    .sign({ key: privateKey, padding: crypto.constants.RSA_PKCS1_PADDING }, "base64url");
  return `${header}.${payload}.${signature}`;
}

const tokenSchema = Schema.Struct({
  token: Schema.String.check(Schema.isMinLength(1)),
  expires_at: githubTimestampSchema,
  permissions: Schema.optional(Schema.Record(Schema.String, Schema.String)),
});
const installationSchema = Schema.Struct({ html_url: Schema.String, permissions: Schema.Record(Schema.String, Schema.String) });
const repositorySchema = Schema.Struct({ full_name: Schema.String, default_branch: Schema.String });
const workflowSchema = Schema.Struct({ path: Schema.String, state: Schema.String });
const contentsSchema = Schema.Struct({ type: Schema.String, encoding: Schema.optional(Schema.String), content: Schema.optional(Schema.String) });
const branchSchema = Schema.Struct({ object: Schema.Struct({ type: Schema.String, sha: Schema.String }) });
const runSchema = Schema.Struct({
  id: githubIdSchema,
  html_url: Schema.String,
  head_sha: Schema.String,
  event: Schema.String,
  path: Schema.String,
  status: Schema.NullOr(Schema.String),
  conclusion: Schema.NullOr(Schema.String),
});
const runsSchema = Schema.Struct({ workflow_runs: Schema.Array(runSchema) });
const dispatchedSchema = Schema.Struct({ workflow_run_id: githubIdSchema, html_url: Schema.String });
const checkRunsSchema = Schema.Struct({
  check_runs: Schema.Array(Schema.Struct({
    id: githubIdSchema,
    external_id: Schema.optional(Schema.NullOr(Schema.String)),
    app: Schema.optional(Schema.NullOr(Schema.Struct({ id: Schema.Number }))),
  })),
});
const checkRunSchema = Schema.Struct({ id: githubIdSchema });
const userAccessTokenSchema = Schema.Struct({ access_token: Schema.String.check(Schema.isMinLength(1)) });
const userSchema = Schema.Struct({ login: Schema.String.check(Schema.isMinLength(1)) });
const userInstallationsSchema = Schema.Struct({
  installations: Schema.Array(Schema.Struct({
    id: githubIdSchema,
    account: Schema.Struct({ login: Schema.String.check(Schema.isMinLength(1)), type: Schema.String, avatar_url: Schema.NullOr(Schema.String) }),
    suspended_at: Schema.NullOr(Schema.String),
  })),
});
const membershipSchema = Schema.Struct({ state: Schema.String, role: Schema.String });

const CHECK_PAGES = 10;
const INSTALLATION_PAGES = 10;

const runOf = (run: typeof runSchema.Type): GithubBuildRun =>
  ({ id: run.id, url: run.html_url, headSha: run.head_sha, event: run.event, status: run.status, conclusion: run.conclusion });

const encodeBranch = (name: string) => name.split("/").map(encodeURIComponent).join("/");

/** The App's authority held in this Cloud, from `GITHUB_APP_PRIVATE_KEY` and `GITHUB_APP_CLIENT_SECRET`. */
export const makeGithubAuthorityLocal = Effect.fn("GithubAuthority.makeLocal")(function* (secrets: {
  readonly appPrivateKey: Redacted.Redacted<string>; readonly appClientSecret: Redacted.Redacted<string>;
}) {
  const config = yield* AppConfig;
  const { origins, appId, appClientId } = config.github;
  const cloud = config.app.url.origin;

  const send = Effect.fn("GithubAuthority.send")(function* (input: {
    operation: GithubObservationOperation; token: string | null; path: `/${string}`; method?: "POST" | "PATCH"; body?: unknown; origin?: string; accept?: string;
  }) {
    const headers = new Headers({ Accept: input.accept ?? "application/vnd.github+json", "X-GitHub-Api-Version": GITHUB_API_VERSION });
    if (input.token !== null) headers.set("Authorization", `Bearer ${input.token}`);
    const init: RequestInit = { headers };
    if (input.method !== undefined) {
      headers.set("Content-Type", "application/json");
      init.method = input.method;
      init.body = JSON.stringify(input.body ?? {});
    }
    const response = yield* Effect.tryPromise({
      try: (signal) => fetch(`${input.origin ?? origins.api}${input.path}`, { ...init, signal }),
      catch: () => failure({ code: "request_failed", operation: input.operation, retriable: true }),
    });
    if (!response.ok) {
      yield* Effect.promise(async () => response.body?.cancel());
      return yield* githubResponseError(response, input.operation);
    }
    return yield* Effect.tryPromise({ try: () => response.text(), catch: () => invalidResponse(input.operation) });
  });

  const call = <S extends Schema.ConstraintDecoder<unknown>>(schema: S, input: Parameters<typeof send>[0]) =>
    send(input).pipe(Effect.flatMap((text) => decodeAs(schema, text, input.operation)));

  const appJwt = Effect.try({
    try: () => createGithubAppJwt({ appId, privateKey: Redacted.value(secrets.appPrivateKey) }),
    catch: () => failure({ code: "request_failed", operation: "installation_token", retriable: false }),
  });

  /**
   * GitHub answers a token mint under a secondary limit with a bare 403, and can cut its body short: both pass, so
   * the mint is tried again rather than failed.
   */
  const mint = Effect.fn("GithubAuthority.mint")(function* (installationId: number, body: {
    permissions: Readonly<Record<string, "read" | "write">>; repository_ids?: ReadonlyArray<number>;
  }) {
    const operation = "installation_token";
    const jwt = yield* appJwt;
    const response = yield* Effect.tryPromise({
      try: (signal) => fetch(`${origins.api}/app/installations/${installationId}/access_tokens`, {
        signal,
        method: "POST",
        headers: { Authorization: `Bearer ${jwt}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": GITHUB_API_VERSION, "Content-Type": "application/json" },
        body: JSON.stringify(body),
      }),
      catch: () => failure({ code: "request_failed", operation, retriable: true }),
    });
    if (!response.ok) {
      yield* Effect.promise(async () => response.body?.cancel());
      const answered = githubResponseError(response, operation);
      return yield* failure({
        code: answered.code, operation, status: response.status, retryAfterSeconds: answered.retryAfterSeconds,
        retriable: response.status === 403 || response.status === 429 || response.status >= 500,
      });
    }
    const cutShort = () => failure({ code: "request_failed", operation, status: response.status, retriable: true });
    const text = yield* Effect.tryPromise({ try: () => response.text(), catch: cutShort });
    const parsed = yield* Effect.try({ try: () => JSON.parse(text), catch: cutShort });
    return yield* Schema.decodeUnknownEffect(tokenSchema)(parsed).pipe(Effect.mapError(() => invalidResponse(operation)));
  });

  /** A token for one repository with what the operation needs, kept until shortly before it expires. Cloud never hands it on. */
  const owned = yield* Cache.makeWith(
    (key: GithubRepositoryRef & { readonly need: OwnNeed }) =>
      mint(key.installationId, { permissions: OWN_PERMISSIONS[key.need], repository_ids: [key.repositoryId] }).pipe(Effect.map((minted) => minted.token)),
    { capacity: 1024, timeToLive: (exit) => Exit.isSuccess(exit) ? OWN_TOKEN_TTL : Duration.zero },
  );
  const own = (ref: GithubRepositoryRef, need: OwnNeed) =>
    Cache.get(owned, { installationId: ref.installationId, repositoryId: ref.repositoryId, need });

  /** A file's bytes at a ref, or null when the ref holds no such file. */
  const file = (token: string, repositoryId: number, path: string, ref: string) => call(contentsSchema, {
    operation: "fetch_contents", token, path: `/repositories/${repositoryId}/contents/${path}?ref=${encodeURIComponent(ref)}`,
  }).pipe(
    Effect.map((found) => found.type === "file" && found.encoding === "base64" && found.content !== undefined
      ? Buffer.from(found.content, "base64") : null),
    Effect.catchIf((error) => error.code === "not_found", () => Effect.succeed(null)),
  );

  const buildRunOf = Effect.fn("GithubAuthority.buildRunOf")(function* (token: string, input: GithubBuildRunRef, operation: GithubObservationOperation) {
    const run = yield* call(runSchema, { operation, token, path: `/repositories/${input.repositoryId}/actions/runs/${input.runId}` });
    if (run.path !== BUILD_WORKFLOW_PATH) return yield* refused(operation, "the run is not of the build workflow");
    return run;
  });

  const installation: GithubAuthorityService["installation"] = Effect.fn("GithubAuthority.local.installation")(function* (installationId) {
    const found = yield* call(installationSchema, { operation: "fetch_installation", token: yield* appJwt, path: `/app/installations/${installationId}` });
    return { url: found.html_url, permissions: found.permissions };
  });

  const readToken: GithubAuthorityService["readToken"] = (installationId, scope) => readSetToken(installationId, scope, installation, (permissions) =>
    Effect.gen(function* () {
      const minted = yield* mint(installationId, { permissions, repository_ids: scope?.repositoryIds });
      const beyond = Object.entries(minted.permissions ?? {}).some(([name, level]) => !Object.hasOwn(GITHUB_READ_SET, name) || level !== "read");
      if (beyond) return yield* failure({ code: "invalid_response", operation: "installation_token", retriable: false });
      return minted.token;
    }));

  const buildWorkflow: GithubAuthorityService["buildWorkflow"] = Effect.fn("GithubAuthority.local.buildWorkflow")(function* (input) {
    const token = yield* own(input, "workflow");
    const repository = yield* call(repositorySchema, { operation: "resolve_repository", token, path: `/repositories/${input.repositoryId}` });
    const workflow = yield* call(workflowSchema, {
      operation: "fetch_workflow", token, path: `/repositories/${input.repositoryId}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}`,
    }).pipe(Effect.catchIf((error) => error.code === "not_found", () => Effect.succeed(null)));
    const bytes = yield* file(token, input.repositoryId, BUILD_WORKFLOW_PATH, repository.default_branch);
    return {
      fullName: repository.full_name,
      defaultBranch: repository.default_branch,
      state: workflow !== null && workflow.path === BUILD_WORKFLOW_PATH ? workflow.state : null,
      canonical: bytes !== null && GITHUB_BUILD_WORKFLOW_DIGESTS.has(githubWorkflowDigest(bytes)),
    };
  });

  const buildRuns: GithubAuthorityService["buildRuns"] = Effect.fn("GithubAuthority.local.buildRuns")(function* (input) {
    const query = new URLSearchParams({ head_sha: input.headSha, event: input.event, per_page: "20" });
    const runs = yield* call(runsSchema, {
      operation: "list_push_runs",
      token: yield* own(input, "runs"),
      path: `/repositories/${input.repositoryId}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}/runs?${query.toString()}`,
    });
    return runs.workflow_runs.filter((run) => run.path === BUILD_WORKFLOW_PATH).map(runOf);
  });

  const buildRun: GithubAuthorityService["buildRun"] = Effect.fn("GithubAuthority.local.buildRun")(function* (input) {
    return runOf(yield* buildRunOf(yield* own(input, "runs"), input, "fetch_run"));
  });

  const dispatchBuild: GithubAuthorityService["dispatchBuild"] = Effect.fn("GithubAuthority.local.dispatchBuild")(function* (input) {
    const operation = "dispatch_workflow";
    const token = yield* own(input, "dispatch");
    const repository = yield* call(repositorySchema, { operation: "resolve_repository", token, path: `/repositories/${input.repositoryId}` });
    const branch = yield* call(branchSchema, {
      operation: "resolve_branch_head", token, path: `/repositories/${input.repositoryId}/git/ref/heads/${encodeBranch(input.branch)}`,
    }).pipe(Effect.catchIf((error) => error.code === "not_found", () => refused(operation, "branch not found")));
    if (branch.object.type !== "commit") return yield* refused(operation, "branch does not point at a commit");
    const head = branch.object.sha;
    const bytes = yield* file(token, input.repositoryId, BUILD_WORKFLOW_PATH, head);
    if (bytes === null) return yield* refused(operation, "the branch has no build workflow");
    if (!GITHUB_BUILD_WORKFLOW_DIGESTS.has(githubWorkflowDigest(bytes))) return yield* refused(operation, "the build workflow is not a canonical template");
    const inputs = Object.fromEntries(Object.entries(input.inputs).filter((entry): entry is [string, string] => entry[1] !== undefined));
    const run = yield* call(dispatchedSchema, {
      operation, token, method: "POST", path: `/repositories/${input.repositoryId}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}/dispatches`,
      body: { ref: `refs/heads/${input.branch}`, inputs: { ...inputs, cloud }, return_run_details: true },
    });
    return { runId: run.workflow_run_id, runUrl: run.html_url, headSha: head, workflowRef: `${repository.full_name}/${BUILD_WORKFLOW_PATH}@refs/heads/${input.branch}` };
  });

  const dispatchApprovedBuild: GithubAuthorityService["dispatchApprovedBuild"] = Effect.fn("GithubAuthority.local.dispatchApprovedBuild")(function* (input) {
    const run = yield* call(dispatchedSchema, {
      operation: "dispatch_workflow",
      token: yield* own(input, "act"),
      method: "POST",
      path: `/repositories/${input.repositoryId}/actions/workflows/${GITHUB_BUILD_WORKFLOW_FILE}/dispatches`,
      body: { ref: `refs/heads/${input.branch}`, inputs: { commit: input.commit }, return_run_details: true },
    });
    return { runId: run.workflow_run_id, runUrl: run.html_url };
  });

  const cancelBuild: GithubAuthorityService["cancelBuild"] = Effect.fn("GithubAuthority.local.cancelBuild")(function* (input) {
    const token = yield* own(input, "act");
    yield* buildRunOf(token, input, "cancel_run");
    yield* send({ operation: "cancel_run", token, method: "POST", path: `/repositories/${input.repositoryId}/actions/runs/${input.runId}/cancel` }).pipe(
      Effect.catchIf((error) => error.status === 409, () => Effect.void),
    );
  });

  const rerunBuild: GithubAuthorityService["rerunBuild"] = Effect.fn("GithubAuthority.local.rerunBuild")(function* (input) {
    const token = yield* own(input, "act");
    yield* buildRunOf(token, input, "rerun_run");
    yield* send({ operation: "rerun_run", token, method: "POST", path: `/repositories/${input.repositoryId}/actions/runs/${input.runId}/rerun` });
  });

  const postCheck: GithubAuthorityService["postCheck"] = Effect.fn("GithubAuthority.local.postCheck")(function* (input) {
    const token = yield* own(input, "check");
    const repository = `/repositories/${input.repositoryId}` as const;
    let existing: number | undefined;
    for (let page = 1; existing === undefined && page <= CHECK_PAGES; page++) {
      const query = new URLSearchParams({ check_name: input.name, filter: "all", per_page: "100", page: String(page) });
      const { check_runs: runs } = yield* call(checkRunsSchema, {
        operation: "list_check_runs", token, path: `${repository}/commits/${input.headSha}/check-runs?${query.toString()}`,
      });
      existing = runs.find((run) => String(run.app?.id) === appId && (run.external_id ?? undefined) === input.externalId)?.id;
      if (runs.length < 100) break;
    }
    const body = {
      name: input.name, external_id: input.externalId, status: "completed", conclusion: input.conclusion, details_url: input.detailsUrl,
      output: { title: input.title, summary: input.summary },
    };
    const posted = existing === undefined
      ? yield* call(checkRunSchema, { operation: "create_check_run", token, method: "POST", path: `${repository}/check-runs`, body: { ...body, head_sha: input.headSha } })
      : yield* call(checkRunSchema, { operation: "update_check_run", token, method: "PATCH", path: `${repository}/check-runs/${existing}`, body });
    return { checkRunId: posted.id };
  });

  const connect: GithubAuthorityService["connect"] = Effect.fn("GithubAuthority.local.connect")(function* (code) {
    const exchanged = yield* call(userAccessTokenSchema, {
      // The OAuth endpoint answers form-encoded unless asked for exactly this.
      operation: "exchange_user_code", token: null, origin: origins.web, method: "POST", path: "/login/oauth/access_token", accept: "application/json",
      body: { client_id: appClientId, client_secret: Redacted.value(secrets.appClientSecret), code },
    });
    const token = exchanged.access_token;
    const { login } = yield* call(userSchema, { operation: "list_user_installations", token, path: "/user" });
    const reached: Array<typeof userInstallationsSchema.Type["installations"][number]> = [];
    for (let page = 1; page <= INSTALLATION_PAGES; page++) {
      const listed = yield* call(userInstallationsSchema, { operation: "list_user_installations", token, path: `/user/installations?per_page=100&page=${page}` });
      reached.push(...listed.installations);
      if (listed.installations.length < 100) break;
    }
    const administers = (account: { login: string; type: string }) => account.type === "Organization"
      ? call(membershipSchema, { operation: "read_user_membership", token, path: `/user/memberships/orgs/${encodeURIComponent(account.login)}` }).pipe(
        Effect.map((membership): boolean | null => membership.state === "active" && membership.role === "admin"),
        Effect.catchIf((error) => error.code === "not_found", () => Effect.succeed(false)),
        Effect.orElseSucceed(() => null),
      )
      : Effect.succeed(account.type === "User" && account.login.toLowerCase() === login.toLowerCase());
    const installations = yield* Effect.forEach(reached, (found) => administers(found.account).pipe(Effect.map((owned) => ({
      id: found.id,
      account: { login: found.account.login, type: found.account.type, avatarUrl: found.account.avatar_url },
      suspendedAt: found.suspended_at,
      administers: owned,
    }))), { concurrency: 8 });
    return { login, installations };
  });

  return {
    readToken, installation, buildWorkflow, buildRuns, buildRun, dispatchBuild, dispatchApprovedBuild, cancelBuild, rerunBuild, postCheck, connect,
  } satisfies GithubAuthorityService;
});

const minterRefusalSchema = Schema.Struct({ error: Schema.Literal("refused"), reason: Schema.String });
const minterGithubSchema = Schema.Struct({
  error: Schema.Literal("github"),
  operation: Schema.String,
  status: Schema.NullOr(Schema.Number),
  retry_after: Schema.optional(Schema.Number),
});
const minterTokenSchema = Schema.Struct({ token: Schema.String.check(Schema.isMinLength(1)), expires_at: Schema.String });
const minterInstallationSchema = Schema.Struct({ url: Schema.String, permissions: Schema.Record(Schema.String, Schema.String) });
const minterWorkflowSchema = Schema.Struct({
  full_name: Schema.String, default_branch: Schema.String, state: Schema.NullOr(Schema.String), digest: Schema.NullOr(Schema.String), canonical: Schema.Boolean,
});
const minterListedRunSchema = Schema.Struct({ id: githubIdSchema, url: Schema.String, head_sha: Schema.String, event: Schema.String, status: Schema.NullOr(Schema.String) });
const minterRunsSchema = Schema.Struct({ runs: Schema.Array(minterListedRunSchema) });
const minterRunSchema = Schema.Struct({ ...minterListedRunSchema.fields, conclusion: Schema.NullOr(Schema.String) });
const minterDispatchSchema = Schema.Struct({ run_id: githubIdSchema, run_url: Schema.String, head_sha: Schema.String, workflow_ref: Schema.String });
const minterCheckSchema = Schema.Struct({ check_run_id: githubIdSchema });
const minterConnectSchema = Schema.Struct({
  login: Schema.String.check(Schema.isMinLength(1)),
  installations: Schema.Array(Schema.Struct({
    id: githubIdSchema,
    account: Schema.Struct({ login: Schema.String, type: Schema.String, avatar_url: Schema.NullOr(Schema.String) }),
    suspended_at: Schema.NullOr(Schema.String),
    administers: Schema.NullOr(Schema.Boolean),
  })),
});

/** A minter route's JSON body: ids, names and inputs Cloud chose. JSON drops a field left undefined. */
type MinterBody = Readonly<Record<string, string | number | ReadonlyArray<number> | GithubReadPermissions | Readonly<Record<string, string | undefined>> | undefined>>;

/** The minter answers a route within GitHub's own calls; past this, Cloud reads it as down. */
const MINTER_TIMEOUT = "30 seconds";

/** The minter's answer when it refused or GitHub failed, as the error a direct GitHub call would have given. */
function minterError(operation: GithubObservationOperation, status: number, text: string): GithubObservationError {
  const refusal = Schema.decodeUnknownOption(Schema.fromJsonString(minterRefusalSchema))(text);
  if (status === 400 && refusal._tag === "Some") return refused(operation, refusal.value.reason.slice(0, 200));
  const github = Schema.decodeUnknownOption(Schema.fromJsonString(minterGithubSchema))(text);
  if (status === 502 && github._tag === "Some") {
    const { status: githubStatus, retry_after: retryAfter } = github.value;
    const minting = github.value.operation === "installation_token";
    // A token mint under a secondary limit answers a bare 403, so any 403 on a mint is worth trying again.
    const limited = githubStatus === 403 && (minting || retryAfter !== undefined);
    // No status: GitHub answered something the minter couldn't read, or a token wider than asked. Waiting fixes neither.
    if (githubStatus === null) return failure({ code: "invalid_response", operation: minting ? "installation_token" : operation, retriable: false });
    return failure({
      code: githubStatus === 404 ? "not_found" : "request_failed",
      operation: minting ? "installation_token" : operation,
      status: githubStatus,
      retryAfterSeconds: retryAfter,
      retriable: githubStatus === 429 || githubStatus >= 500 || limited,
    });
  }
  // The minter itself is down, throttled, or refuses Cloud's credential, which waiting won't fix.
  return failure({ code: "request_failed", operation, retriable: status === 429 || status >= 500 });
}

/** A signed POST to the minter: SigV4 for the Function URL, or a verify lab's bearer. */
function minterRequest(auth: GithubMinterAuth): (url: string, body: string) => Promise<Request> {
  const headers = { "Content-Type": "application/json" };
  if (auth.kind === "bearer") {
    return (url, body) => Promise.resolve(new Request(url, {
      method: "POST", body, headers: { ...headers, Authorization: `Bearer ${Redacted.value(auth.token)}` },
    }));
  }
  const client = new AwsClient({
    accessKeyId: auth.accessKeyId, secretAccessKey: Redacted.value(auth.secretAccessKey), service: "lambda", region: auth.region,
  });
  return (url, body) => client.sign(url, { method: "POST", body, headers });
}

/** The App's authority held by the minter. Cloud holds only the credential that asks it. */
export function makeGithubAuthorityMinter(minter: { readonly url: URL; readonly auth: GithubMinterAuth }): GithubAuthorityService {
  const request = minterRequest(minter.auth);

  const post = <S extends Schema.ConstraintDecoder<unknown>>(route: `/v1/${string}`, operation: GithubObservationOperation, body: MinterBody, schema: S) =>
    Effect.gen(function* () {
      const answered = yield* Effect.tryPromise({
        try: async (signal) => {
          const response = await fetch(await request(new URL(route, minter.url).toString(), JSON.stringify(body)), { signal, redirect: "error" });
          return { status: response.status, text: await response.text() };
        },
        catch: () => failure({ code: "request_failed", operation, retriable: true }),
      }).pipe(
        Effect.timeoutOrElse({ duration: MINTER_TIMEOUT, orElse: () => Effect.fail(failure({ code: "request_failed", operation, retriable: true })) }),
      );
      if (answered.status !== 200) return yield* minterError(operation, answered.status, answered.text);
      return yield* decodeAs(schema, answered.text, operation);
    });

  const installation: GithubAuthorityService["installation"] = (installationId) =>
    post("/v1/installation", "fetch_installation", { installation_id: installationId }, minterInstallationSchema);

  const readToken: GithubAuthorityService["readToken"] = (installationId, scope) => readSetToken(installationId, scope, installation, (permissions) =>
    post("/v1/token", "installation_token", { installation_id: installationId, repository_ids: scope?.repositoryIds, permissions }, minterTokenSchema).pipe(
      Effect.map((minted) => minted.token),
    ));

  return {
    readToken,
    installation,
    buildWorkflow: (input) => post("/v1/workflow", "fetch_workflow", { installation_id: input.installationId, repository_id: input.repositoryId }, minterWorkflowSchema).pipe(
      Effect.map((found) => ({ fullName: found.full_name, defaultBranch: found.default_branch, state: found.state, canonical: found.canonical })),
    ),
    buildRuns: (input) => post("/v1/runs", "list_push_runs", {
      installation_id: input.installationId, repository_id: input.repositoryId, head_sha: input.headSha, event: input.event,
    }, minterRunsSchema).pipe(Effect.map(({ runs }) => runs.map((run) => ({
      id: run.id, url: run.url, headSha: run.head_sha, event: run.event, status: run.status, conclusion: null,
    })))),
    buildRun: (input) => post("/v1/run", "fetch_run", { installation_id: input.installationId, repository_id: input.repositoryId, run_id: input.runId }, minterRunSchema).pipe(
      Effect.map((run) => ({ id: run.id, url: run.url, headSha: run.head_sha, event: run.event, status: run.status, conclusion: run.conclusion })),
    ),
    dispatchBuild: (input) => post("/v1/dispatch", "dispatch_workflow", {
      installation_id: input.installationId, repository_id: input.repositoryId, branch: input.branch, inputs: input.inputs,
    }, minterDispatchSchema).pipe(Effect.map((run) => ({ runId: run.run_id, runUrl: run.run_url, headSha: run.head_sha, workflowRef: run.workflow_ref }))),
    dispatchApprovedBuild: () => Effect.fail(refused("dispatch_workflow", "the minter dispatches only the build workflow Ployz offers")),
    cancelBuild: (input) => post("/v1/cancel", "cancel_run", { installation_id: input.installationId, repository_id: input.repositoryId, run_id: input.runId }, Schema.Struct({})).pipe(
      Effect.asVoid,
    ),
    rerunBuild: () => Effect.fail(refused("rerun_run", "the minter runs no build again")),
    postCheck: (input) => post("/v1/check", "create_check_run", {
      installation_id: input.installationId,
      repository_id: input.repositoryId,
      head_sha: input.headSha,
      name: input.name,
      external_id: input.externalId,
      status: "completed",
      conclusion: input.conclusion,
      details_url: input.detailsUrl,
      output: { title: input.title, summary: input.summary },
    }, minterCheckSchema).pipe(Effect.map((posted) => ({ checkRunId: posted.check_run_id }))),
    connect: (code) => post("/v1/connect", "exchange_user_code", { code }, minterConnectSchema).pipe(Effect.map((found) => ({
      login: found.login,
      installations: found.installations.map((installation) => ({
        id: installation.id,
        account: { login: installation.account.login, type: installation.account.type, avatarUrl: installation.account.avatar_url },
        suspendedAt: installation.suspended_at,
        administers: installation.administers,
      })),
    }))),
  };
}

export const GithubAuthorityLocal = Layer.effect(GithubAuthority, Effect.gen(function* () {
  const { authority } = (yield* AppConfig).github;
  if (authority.mode !== "local") return yield* Effect.die("GithubAuthorityLocal needs GITHUB_APP_PRIVATE_KEY and GITHUB_APP_CLIENT_SECRET");
  return yield* makeGithubAuthorityLocal(authority);
}));

export const GithubAuthorityMinter = Layer.effect(GithubAuthority, Effect.gen(function* () {
  const { authority } = (yield* AppConfig).github;
  if (authority.mode !== "minter") return yield* Effect.die("GithubAuthorityMinter needs GITHUB_MINTER_URL");
  return makeGithubAuthorityMinter(authority);
}));

/** The minter when `GITHUB_MINTER_URL` is set, else this Cloud's own key. */
export const GithubAuthorityLive = Layer.effect(GithubAuthority, Effect.gen(function* () {
  const { authority } = (yield* AppConfig).github;
  return authority.mode === "minter" ? makeGithubAuthorityMinter(authority) : yield* makeGithubAuthorityLocal(authority);
}));
