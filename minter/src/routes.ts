import type { AuditFacts, AuditLine, AuditSink } from "./audit.js";
import { field, Github, GithubError, isRecord, nullable, type TokenPermissions } from "./github.js";
import { branch, httpsUrl, id, ids, object, oneOf, optional, Refusal, sha, strict, text, type Parsed, type Shape } from "./parse.js";
import {
  BUILD_WORKFLOW_FILE,
  BUILD_WORKFLOW_PATH,
  PLOYZ_CHECK_NAMES,
  READ_SET,
  readTokenPermissions,
  workflowDigest,
} from "./policy.js";

export interface MinterConfig {
  appId: string;
  clientId: string;
  clientSecret: string;
  /** Cloud's origin, the `cloud` input of every dispatch. Cloud never sets it. */
  cloud: string;
  /** Digests of the `ployz-build.yml` files the minter dispatches. */
  canonical: ReadonlySet<string>;
}

export interface MinterRequest {
  method: string;
  path: string;
  /** Who the transport authenticated: the IAM principal, or the local bearer. Null when it authenticated no one. */
  caller: string | null;
  body: unknown;
}

export interface MinterResponse {
  status: number;
  body: Record<string, unknown>;
}

interface Context {
  github: Github;
  config: MinterConfig;
  facts: AuditFacts;
  /** The minter's own token for the request's one repository, with the route's `own` permissions. Never returned. */
  own(): Promise<string>;
}

interface Route<S extends Shape> {
  body: S;
  own?: TokenPermissions;
  handle(body: Parsed<S>, context: Context): Promise<Record<string, unknown>>;
}

type AnyRoute = Route<Shape>;
const route = <S extends Shape>(row: Route<S>) => row as unknown as AnyRoute;

const BUILD_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]*$/;
const RUNNER = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;
const EVENT = /^[a-z_]+$/;
const CONNECT_CODE = /^[A-Za-z0-9_-]+$/;
const CHECK_CONCLUSIONS = ["success", "neutral", "failure", "cancelled", "skipped", "timed_out", "action_required"] as const;
const CHECK_PAGES = 10;
const INSTALLATION_PAGES = 10;

/**
 * Every route the minter serves, and the whole of what Cloud can ask it. `own` is the token the minter mints for
 * itself to do the route's work on one repository. No route returns that token, takes a GitHub path, or reads
 * Actions jobs, logs or artifacts.
 */
export const ROUTES: Readonly<Record<string, AnyRoute>> = {
  "/v1/token": route({
    body: { installation_id: id, repository_ids: optional(ids(100)), permissions: optional(object) },
    async handle(body, { github, facts }) {
      if (body.repository_ids !== undefined) facts.repository_ids = body.repository_ids;
      if (body.permissions !== undefined) facts.permissions_asked = askedForAudit(body.permissions);
      const decided = readTokenPermissions(body.permissions);
      if (!decided.ok) throw new Refusal(decided.reason);
      const minted = await github.mint(body.installation_id, {
        permissions: decided.permissions,
        ...(body.repository_ids === undefined ? {} : { repositoryIds: body.repository_ids }),
      });
      facts.permissions_granted = minted.permissions;
      facts.token_expires_at = minted.expiresAt;
      const beyond = Object.entries(minted.permissions).find(([name, level]) => !Object.hasOwn(READ_SET, name) || level !== "read");
      if (beyond) throw new GithubError("installation_token_granted_more", null);
      return { token: minted.token, expires_at: minted.expiresAt };
    },
  }),

  "/v1/installation": route({
    body: { installation_id: id },
    async handle(body, { github }) {
      const data = await github.app("fetch_installation", `/app/installations/${body.installation_id}`);
      return { url: field(data, "html_url", "string", "fetch_installation"), permissions: field(data, "permissions", "object", "fetch_installation") };
    },
  }),

  "/v1/workflow": route({
    body: { installation_id: id, repository_id: id },
    own: { contents: "read", actions: "read" },
    async handle(body, { github, config, facts, own }) {
      const token = await own();
      const repository = await github.json("resolve_repository", token, "GET", `/repositories/${body.repository_id}`);
      const defaultBranch = field(repository, "default_branch", "string", "resolve_repository");
      const workflow = await github.send("fetch_workflow", token, "GET", `/repositories/${body.repository_id}/actions/workflows/${BUILD_WORKFLOW_FILE}`, { allow: [404] });
      let state: string | null = null;
      if (workflow.status === 404) await workflow.body?.cancel();
      else {
        const data: unknown = await workflow.json();
        if (field(data, "path", "string", "fetch_workflow") === BUILD_WORKFLOW_PATH) state = field(data, "state", "string", "fetch_workflow");
      }
      const file = await github.file(token, `/repositories/${body.repository_id}/contents/${BUILD_WORKFLOW_PATH}?ref=${encodeURIComponent(defaultBranch)}`);
      const digest = file === null ? null : workflowDigest(file);
      facts.ref = defaultBranch;
      if (digest !== null) facts.workflow_digest = digest;
      return {
        full_name: field(repository, "full_name", "string", "resolve_repository"),
        default_branch: defaultBranch,
        state,
        digest,
        canonical: digest !== null && config.canonical.has(digest),
      };
    },
  }),

  "/v1/runs": route({
    body: { installation_id: id, repository_id: id, head_sha: sha, event: text(32, EVENT) },
    own: { actions: "read" },
    async handle(body, { github, facts, own }) {
      facts.head_sha = body.head_sha;
      const query = new URLSearchParams({ head_sha: body.head_sha, event: body.event, per_page: "20" });
      const data = await github.json("list_runs", await own(), "GET", `/repositories/${body.repository_id}/actions/workflows/${BUILD_WORKFLOW_FILE}/runs?${query.toString()}`);
      const runs = field(data, "workflow_runs", "array", "list_runs").filter(isBuildRun).map((run) => {
        const { conclusion: _, ...listed } = runFields(run, "list_runs");
        return listed;
      });
      return { runs };
    },
  }),

  "/v1/run": route({
    body: { installation_id: id, repository_id: id, run_id: id },
    own: { actions: "read" },
    async handle(body, { github, facts, own }) {
      facts.run_id = body.run_id;
      return runFields(await buildRun(github, await own(), body.repository_id, body.run_id), "fetch_run");
    },
  }),

  "/v1/dispatch": route({
    body: { installation_id: id, repository_id: id, branch, inputs: object },
    own: { contents: "read", actions: "write" },
    async handle(body, { github, config, facts, own }) {
      facts.ref = body.branch;
      const inputs = strict(body.inputs, { build: text(256, BUILD_ID), runner: optional(text(100, RUNNER)), commit: optional(sha) }, "inputs");
      const token = await own();
      const repository = await github.json("resolve_repository", token, "GET", `/repositories/${body.repository_id}`);
      const fullName = field(repository, "full_name", "string", "resolve_repository");
      const ref = await github.send("read_branch", token, "GET", `/repositories/${body.repository_id}/git/ref/heads/${encodeBranch(body.branch)}`, { allow: [404] });
      if (ref.status === 404) {
        await ref.body?.cancel();
        throw new Refusal("branch not found");
      }
      const target = field(await ref.json(), "object", "object", "read_branch");
      if (field(target, "type", "string", "read_branch") !== "commit") throw new Refusal("branch does not point at a commit");
      const head = field(target, "sha", "string", "read_branch");
      facts.head_sha = head;
      const file = await github.file(token, `/repositories/${body.repository_id}/contents/${BUILD_WORKFLOW_PATH}?ref=${head}`);
      if (file === null) throw new Refusal("the branch has no build workflow");
      const digest = workflowDigest(file);
      facts.workflow_digest = digest;
      if (!config.canonical.has(digest)) throw new Refusal("the build workflow is not a canonical template");
      const run = await github.json("dispatch_workflow", token, "POST", `/repositories/${body.repository_id}/actions/workflows/${BUILD_WORKFLOW_FILE}/dispatches`, {
        ref: `refs/heads/${body.branch}`,
        inputs: { ...definedInputs(inputs), cloud: config.cloud },
        return_run_details: true,
      });
      const runId = field(run, "workflow_run_id", "number", "dispatch_workflow");
      facts.run_id = runId;
      return {
        run_id: runId,
        run_url: field(run, "html_url", "string", "dispatch_workflow"),
        head_sha: head,
        workflow_ref: `${fullName}/${BUILD_WORKFLOW_PATH}@refs/heads/${body.branch}`,
      };
    },
  }),

  "/v1/cancel": route({
    body: { installation_id: id, repository_id: id, run_id: id },
    own: { actions: "write" },
    async handle(body, { github, facts, own }) {
      facts.run_id = body.run_id;
      const token = await own();
      await buildRun(github, token, body.repository_id, body.run_id);
      const cancelled = await github.send("cancel_run", token, "POST", `/repositories/${body.repository_id}/actions/runs/${body.run_id}/cancel`, { allow: [409] });
      await cancelled.body?.cancel();
      facts.github_status = cancelled.status;
      return {};
    },
  }),

  "/v1/check": route({
    body: {
      installation_id: id,
      repository_id: id,
      head_sha: sha,
      name: text(100),
      external_id: optional(text(200)),
      status: oneOf(["queued", "in_progress", "completed"]),
      conclusion: optional(oneOf(CHECK_CONCLUSIONS)),
      details_url: optional(httpsUrl),
      output: optional(object),
    },
    own: { checks: "write" },
    async handle(body, { github, config, facts, own }) {
      facts.check_name = body.name;
      facts.head_sha = body.head_sha;
      if (!PLOYZ_CHECK_NAMES.has(body.name)) throw new Refusal("check name is not a Ployz check");
      if ((body.status === "completed") !== (body.conclusion !== undefined)) throw new Refusal("conclusion goes with a completed status, and only with it");
      const output = body.output === undefined ? undefined : strict(body.output, { title: text(1000), summary: text(65535) }, "output");
      const token = await own();
      const repository = `/repositories/${body.repository_id}` as const;
      let existing: number | undefined;
      for (let page = 1; existing === undefined && page <= CHECK_PAGES; page++) {
        const query = new URLSearchParams({ check_name: body.name, filter: "all", per_page: "100", page: String(page) });
        const runs = field(await github.json("list_check_runs", token, "GET", `${repository}/commits/${body.head_sha}/check-runs?${query.toString()}`), "check_runs", "array", "list_check_runs");
        // GitHub returns a check posted without an external_id with an empty one.
        const match = runs.find((run) => ownCheckRun(run, config.appId) && (nullable(run, "external_id", "string", "list_check_runs") || undefined) === body.external_id);
        if (match !== undefined) existing = field(match, "id", "number", "list_check_runs");
        if (runs.length < 100) break;
      }
      const payload = {
        name: body.name,
        status: body.status,
        ...(body.conclusion === undefined ? {} : { conclusion: body.conclusion }),
        ...(body.external_id === undefined ? {} : { external_id: body.external_id }),
        ...(body.details_url === undefined ? {} : { details_url: body.details_url }),
        ...(output === undefined ? {} : { output }),
      };
      const posted = existing === undefined
        ? await github.json("create_check_run", token, "POST", `${repository}/check-runs`, { ...payload, head_sha: body.head_sha })
        : await github.json("update_check_run", token, "PATCH", `${repository}/check-runs/${existing}`, payload);
      return { check_run_id: field(posted, "id", "number", existing === undefined ? "create_check_run" : "update_check_run") };
    },
  }),

  "/v1/connect": route({
    body: { code: text(512, CONNECT_CODE) },
    async handle(body, { github, config }) {
      const token = await github.exchangeCode(config.clientId, config.clientSecret, body.code);
      const login = field(await github.json("read_user", token, "GET", "/user"), "login", "string", "read_user");
      const installations: Record<string, unknown>[] = [];
      for (let page = 1; page <= INSTALLATION_PAGES; page++) {
        const listed = field(await github.json("list_user_installations", token, "GET", `/user/installations?per_page=100&page=${page}`), "installations", "array", "list_user_installations");
        for (const installation of listed) {
          const account = field(installation, "account", "object", "list_user_installations");
          installations.push({
            id: field(installation, "id", "number", "list_user_installations"),
            account: {
              login: field(account, "login", "string", "list_user_installations"),
              type: field(account, "type", "string", "list_user_installations"),
              avatar_url: nullable(account, "avatar_url", "string", "list_user_installations"),
            },
            suspended_at: nullable(installation, "suspended_at", "string", "list_user_installations"),
          });
        }
        if (listed.length < 100) break;
      }
      const administers = async (account: { login: string; type: string }) => {
        if (account.type !== "Organization") return account.type === "User" && account.login.toLowerCase() === login.toLowerCase();
        const membership = await github.send("read_user_membership", token, "GET", `/user/memberships/orgs/${encodeURIComponent(account.login)}`, { allow: [404] });
        if (membership.status === 404) {
          await membership.body?.cancel();
          return false;
        }
        const data: unknown = await membership.json();
        return field(data, "state", "string", "read_user_membership") === "active" && field(data, "role", "string", "read_user_membership") === "admin";
      };
      for (let start = 0; start < installations.length; start += 8) {
        await Promise.all(installations.slice(start, start + 8).map(async (installation) => {
          installation["administers"] = await administers(installation["account"] as { login: string; type: string });
        }));
      }
      return { login, installations };
    },
  }),
};

/** What a token request asked for, cut to a size an audit line can hold whatever the caller sent. */
function askedForAudit(asked: Record<string, unknown>): Record<string, string> {
  return Object.fromEntries(Object.entries(asked).slice(0, 20).map(([name, level]) => [name.slice(0, 64), String(level).slice(0, 16)]));
}

function isBuildRun(run: unknown): boolean {
  return isRecord(run) && run["path"] === BUILD_WORKFLOW_PATH;
}

/** A run of the build workflow in this repository, or a refusal: the run routes never touch another workflow's runs. */
async function buildRun(github: Github, token: string, repositoryId: number, runId: number): Promise<unknown> {
  const run = await github.json("fetch_run", token, "GET", `/repositories/${repositoryId}/actions/runs/${runId}`);
  if (!isBuildRun(run)) throw new Refusal("the run is not of the build workflow");
  return run;
}

/** The metadata Cloud may see of a run. Never its jobs, logs, artifacts or the URLs that lead to them. */
function runFields(run: unknown, operation: string) {
  return {
    id: field(run, "id", "number", operation),
    url: field(run, "html_url", "string", operation),
    head_sha: field(run, "head_sha", "string", operation),
    event: field(run, "event", "string", operation),
    status: nullable(run, "status", "string", operation),
    conclusion: nullable(run, "conclusion", "string", operation),
  };
}

function ownCheckRun(run: unknown, appId: string): boolean {
  const app = isRecord(run) ? run["app"] : undefined;
  return isRecord(app) && String(app["id"]) === appId;
}

function encodeBranch(name: string): string {
  return name.split("/").map(encodeURIComponent).join("/");
}

function definedInputs(inputs: Record<string, string | undefined>): Record<string, string> {
  return Object.fromEntries(Object.entries(inputs).filter((entry): entry is [string, string] => entry[1] !== undefined));
}

/** Serves one request and writes its one audit line, whatever happens. */
export function createMinter(deps: { github: Github; config: MinterConfig; audit: AuditSink; now?: () => number }) {
  const now = deps.now ?? Date.now;
  return async function handle(request: MinterRequest): Promise<MinterResponse> {
    const started = now();
    const facts: AuditFacts = {};
    const row = Object.hasOwn(ROUTES, request.path) ? ROUTES[request.path] : undefined;
    let response: MinterResponse;
    let outcome: AuditLine["outcome"] = "granted";
    let reason: string | undefined;
    try {
      if (request.caller === null) {
        outcome = "refused";
        reason = "unauthenticated";
        response = { status: 403, body: { error: "unauthenticated" } };
      } else if (request.method !== "POST") {
        outcome = "refused";
        reason = "method is not POST";
        response = { status: 405, body: { error: "method_not_allowed" } };
      } else if (row === undefined) {
        outcome = "refused";
        reason = "unknown route";
        response = { status: 404, body: { error: "not_found" } };
      } else {
        response = { status: 200, body: await serve(row, request.body, deps, facts) };
      }
    } catch (error) {
      if (error instanceof Refusal) {
        outcome = "refused";
        reason = error.reason;
        response = { status: 400, body: { error: "refused", reason: error.reason } };
      } else if (error instanceof GithubError) {
        outcome = "failed";
        reason = `github ${error.operation}`;
        if (error.status !== null) facts.github_status = error.status;
        response = {
          status: 502,
          body: { error: "github", operation: error.operation, status: error.status, ...(error.retryAfter === null ? {} : { retry_after: error.retryAfter }) },
        };
      } else {
        outcome = "failed";
        reason = "internal";
        response = { status: 500, body: { error: "internal" } };
      }
    }
    deps.audit({
      time: new Date(started).toISOString(),
      caller: request.caller ?? "unauthenticated",
      route: row === undefined ? request.path.slice(0, 64) : request.path,
      outcome,
      ...(reason === undefined ? {} : { reason }),
      ...facts,
      duration_ms: now() - started,
    });
    return response;
  };
}

async function serve(row: AnyRoute, raw: unknown, deps: { github: Github; config: MinterConfig }, facts: AuditFacts) {
  const body = strict(raw, row.body);
  const installationId = body["installation_id"];
  const repositoryId = body["repository_id"];
  if (typeof installationId === "number") facts.installation_id = installationId;
  if (typeof repositoryId === "number") facts.repository_ids = [repositoryId];
  let token: Promise<string> | undefined;
  const own = () => {
    if (row.own === undefined || typeof installationId !== "number" || typeof repositoryId !== "number") {
      throw new Error("this route mints no token of its own");
    }
    const permissions = row.own;
    token ??= deps.github.mint(installationId, { permissions, repositoryIds: [repositoryId] }).then((minted) => minted.token);
    return token;
  };
  return row.handle(body, { github: deps.github, config: deps.config, facts, own });
}
