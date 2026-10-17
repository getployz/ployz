import { assert, it } from "@effect/vitest";
import { expect, vi } from "vitest";
import type { ConfigCommand, ConfigQuery, MachineId } from "@ployz/sdk";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { and, eq, sql } from "drizzle-orm";
import * as tar from "tar";
import { Cause, Effect, Exit, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";
import { runAgentCall } from "#/modules/agent/agent-runner.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { operationApprovals } from "#/modules/approvals/tables";
import { callStore, callStoreAsMember, callStoreBy, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { Access } from "#/modules/organization/access.server";
import * as ids from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { runStoreDeployment } from "#/modules/config-store/store-deployment.server";
import { uploadChunk } from "#/modules/config-store/tables";
import { receiveUpload, releaseUpload } from "#/modules/config-store/upload.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Auth, AuthLive } from "#/server/auth.server";
import { githubInstallation, githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import { member, session as sessionTable, user } from "#/modules/identity/tables";
import { machineRemoveAttempt, organizationMachine } from "#/modules/machines/tables";
import {
  completeMachineRemoveAttemptActivity,
  prepareMachineRemoveAttemptActivity,
  releaseServerActivity,
  removeMachineActivity,
} from "#/modules/machines/machine-removal.server";
import { requestMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { LeftNotForgotten, PloyzProviderError, type PloyzSession } from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { organizationPairing } from "#/modules/runtime/tables";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { asTestDouble } from "#/lib/test-double";
import { Database } from "#/server/database.server";
import { fakeGithubApi } from "#/test/fake-github";
import { encodePublicError, NotFound, statusForPublicError } from "#/server/public-error";

const origin = "http://localhost:3000";

/**
 * GitHub as these tests see it: `acme/web` is private (read through installation 7) with `dev` and `main` branches,
 * `acme/docs` is public, `acme/secret` is someone else's private repository, and `acme/down` finds GitHub unreachable.
 */
const github = fakeGithubApi({
  "/repos/acme/web/git/ref/heads%2Fdev": {
    ref: "refs/heads/dev", object: { type: "commit", sha: "a".repeat(40) },
  },
  "/repos/acme/web/git/ref/heads%2Fmain": {
    ref: "refs/heads/main", object: { type: "commit", sha: "b".repeat(40) },
  },
  "/repos/acme/docs": { id: 12, full_name: "acme/docs", private: false, default_branch: "main" },
  "/repos/acme/secret": { id: 13, full_name: "acme/secret", private: true, default_branch: "main" },
  "/repos/acme/down": "down",
});

/** Cloud with the Config Store in its database. */
const cloudLayer = Effect.fn(function* (
  overrides: { readonly polar?: PolarService; readonly hostedDnsUrl?: string } = {},
  inngest = new Inngest({ id: "config-store-test" }),
) {
  const services = yield* storeTestCloud({
    github: github.service,
    inngest,
    // No Hosted DNS answers unless a test starts one.
    env: { PLOYZ_HOSTED_DNS_URL: overrides.hostedDnsUrl ?? "http://127.0.0.1:9/" },
    polar: overrides.polar ?? { mode: "self_hosted" },
  });
  return Layer.merge(AuthLive.pipe(Layer.provide(services)), services);
});

/** The fields these tests read from Store results and refusals. */
const Reply = Schema.Struct({
  written: Schema.optionalKey(Schema.String),
  view: Schema.optionalKey(Schema.String),
  staged: Schema.optionalKey(Schema.Array(Schema.String)),
  settings: Schema.optionalKey(Schema.Array(Schema.Struct({
    path: Schema.String, value: Schema.Unknown, default: Schema.Unknown, apply: Schema.String,
  }))),
  values: Schema.optionalKey(Schema.Record(Schema.String, Schema.Unknown)),
  error: Schema.optionalKey(Schema.Struct({
    code: Schema.String,
    message: Schema.optionalKey(Schema.String),
    details: Schema.NullOr(Schema.Struct({
      revision: Schema.optionalKey(Schema.Number), next: Schema.optionalKey(Schema.String),
      approval: Schema.optionalKey(Schema.String), approval_id: Schema.optionalKey(Schema.String),
    })),
  })),
  domain: Schema.optionalKey(Schema.Struct({
    kind: Schema.String, prefix: Schema.optionalKey(Schema.String), hostname: Schema.NullOr(Schema.String),
  })),
  domains: Schema.optionalKey(Schema.Array(Schema.Struct({
    hostname: Schema.NullOr(Schema.String), status: Schema.String, action: Schema.NullOr(Schema.Struct({ type: Schema.String })),
  }))),
});
type Reply = typeof Reply.Type;
/** Checks the fields the tests read and keeps the rest, so whole-reply assertions still see every field. */
const decodeReply = (text: string) => Schema.decodeUnknownSync(Reply)(JSON.parse(text), { onExcessProperty: "preserve" });

/** One request carrying `headers`; `json` is the Store's result, or its refusal under `error`. */
const send = Effect.fn(function* (path: string, headers: Headers, body: Body | undefined, method = "POST") {
  headers.set("content-type", "application/json");
  const init: RequestInit = { method, headers };
  if (method === "POST") init.body = JSON.stringify(body);
  const exit = yield* Effect.exit(handleConfigRequest(new Request(`${origin}/api/config/${path}`, init)));
  if (Exit.isFailure(exit)) {
    const empty: Reply = {};
    return { status: statusForPublicError(encodePublicError(Cause.squash(exit.cause))), json: empty };
  }
  const text = yield* Effect.promise(() => exit.value.text());
  return { status: exit.value.status, json: decodeReply(text) };
});

/** One CLI request, signed in as `session` unless it's undefined. */
const request = (path: string, session: string | undefined, body: Body | undefined, method = "POST") =>
  send(path, session === undefined ? new Headers() : signedIn(session), body, method);

/** A new account's session, as the CLI's bearer token and as the browser's cookie. */
const signUpBoth = Effect.fn(function* (name: string) {
  const auth = yield* Auth;
  const response = yield* auth.handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  return {
    session: response.headers.get("set-auth-token") ?? assert.fail("no session token"),
    cookie: response.headers.get("set-cookie")?.split(";", 1)[0] ?? assert.fail("no session cookie"),
  };
});

const signUp = (name: string) => signUpBoth(name).pipe(Effect.map(({ session }) => session));

/** How the signed-in CLI presents `session`. */
const signedIn = (session: string) => new Headers({ authorization: `Bearer ${session}` });

/** What the CLI sends, plus one command the Store never takes over HTTPS. */
type Body = ConfigQuery | ConfigCommand | { readonly command: "claim"; readonly deployment: string }
  | { readonly command: "create_git_service"; readonly installation_id: number };

const PROJECT = "00000000-0000-4000-8000-000000000001";
const ENVIRONMENT = "00000000-0000-4000-8000-000000000002";
const SERVICE = "00000000-0000-4000-8000-000000000003";
const here = { project: null, environment: null };
const shop: ConfigCommand = { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT };
const web: ConfigCommand = {
  command: "create_service", id: SERVICE, environment: here, name: "web", image: "nginx:1",
};
const write = (command: ConfigCommand) => ({ operation: "write" as const, command });
const get = (path: string | null): ConfigQuery => ({ query: "environment", environment: here, path, all: false });

it.live(
  "the CLI reads and writes its own Organization's Config Store over HTTPS",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const bob = yield* signUp("bob");

        assert.strictEqual((yield* request("write", undefined, shop)).status, 401);

        const created = yield* request("write", alice, shop);
        assert.strictEqual(created.status, 200);
        assert.strictEqual(created.json.written, "project");
        // A retried create replays; the same ID with another body conflicts.
        assert.deepStrictEqual((yield* request("write", alice, shop)).json, created.json);
        const reused = yield* request("write", alice, { ...shop, name: "blog" });
        assert.strictEqual(reused.status, 409);
        assert.strictEqual(reused.json.error?.code, "conflict");

        assert.strictEqual((yield* request("write", alice, web)).status, 200);
        const edited = yield* request("write", alice, {
          command: "edit", environment: here, expect: 2, changes: [{ op: "set", path: "web.replicas", value: "3" }],
        });
        assert.deepStrictEqual(edited.json.staged, ["web.replicas"]);
        const replicas = yield* request("read", alice, get("web.replicas"));
        assert.deepInclude(replicas.json, { view: "environment" });
        assert.deepStrictEqual(replicas.json.settings, [
          { path: "web.replicas", value: 3, default: 1, apply: "staged" },
        ]);

        const stale = yield* request("write", alice, {
          command: "edit", environment: here, expect: 2, changes: [{ op: "unset", path: "web.replicas" }],
        });
        assert.strictEqual(stale.status, 409);
        assert.strictEqual(stale.json.error?.code, "conflict");
        assert.deepStrictEqual(stale.json.error?.details, { revision: 3 });

        // Another Organization sees none of it.
        const foreign = yield* request("read", bob, get(null));
        assert.strictEqual(foreign.status, 404);
        assert.strictEqual(foreign.json.error?.code, "not_found");

        const invalid = yield* request("write", alice, { command: "claim", deployment: "d1" });
        assert.strictEqual(invalid.status, 422);
        assert.strictEqual(invalid.json.error?.code, "invalid_argument");
        assert.strictEqual((yield* request("claim", alice, get(null))).status, 404);
        assert.strictEqual((yield* request("read", alice, undefined, "GET")).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a browser's cookie signs no config request in, so a page on another site can't write through it",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUpBoth("alice");
        const forged = new Headers({ cookie: alice.cookie, "sec-fetch-site": "cross-site", origin: "https://evil.example" });
        assert.strictEqual((yield* send("write", forged, shop)).status, 401);
        assert.strictEqual((yield* request("read", alice.session, get(null))).status, 404);
        assert.strictEqual((yield* send("write", new Headers({ cookie: alice.cookie, "sec-fetch-site": "same-origin" }), shop)).status, 401);
        assert.strictEqual((yield* request("write", alice.session, shop)).status, 200);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "the dashboard reaches the Store as a member of the Organization it names, refusals intact",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* resolveCaller(signedIn(yield* signUp("alice")));
        const bob = yield* resolveCaller(signedIn(yield* signUp("bob")));
        const slug = alice.organization.slug;

        assert.isTrue((yield* callStoreAsMember(alice, slug, write(shop))).ok);
        assert.isTrue((yield* callStoreAsMember(alice, slug, write(web))).ok);
        const stale = yield* callStoreAsMember(alice, slug, write({
          command: "edit", environment: here, expect: 1, changes: [{ op: "set", path: "web.replicas", value: 3 }],
        }));
        assert.isFalse(stale.ok);
        if (!stale.ok) {
          assert.strictEqual(stale.refusal.code, "conflict");
          assert.deepStrictEqual(stale.refusal.details, { revision: 2 });
        }
        const view = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web.replicas") });
        assert.deepInclude(view, { ok: true });

        // Naming an Organization you're not a member of is refused before the Store is asked.
        const foreign = yield* Effect.exit(callStoreAsMember(bob, slug, { operation: "read", query: get(null) }));
        assert.isTrue(Exit.isFailure(foreign) && Cause.squash(foreign.cause) instanceof NotFound);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a member stages but can't delete an Environment or a Project; an admin can",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* resolveCaller(signedIn(yield* signUp("alice")));
        const bobSession = yield* signUp("bob");
        const bobUser = (yield* resolveCaller(signedIn(bobSession))).userId;
        const slug = alice.organization.slug;
        const { drizzle } = yield* Database;
        yield* drizzle.insert(member).values({ userId: bobUser, organizationId: alice.organization.id, role: "member" });
        const bob = { userId: bobUser };

        assert.isTrue((yield* callStoreAsMember(alice, slug, write(shop))).ok);
        assert.isTrue((yield* callStoreAsMember(alice, slug, write({
          command: "create_environment", id: "00000000-0000-4000-8000-000000000004", project: "shop", name: "staging",
        }))).ok);
        assert.isTrue((yield* writeStoreAsMember(bob, slug, web)).ok, "a member stages");

        const deleteStaging: ConfigCommand = { command: "remove", project: "shop", environment: "staging" };
        const deleteShop: ConfigCommand = { command: "remove", project: "shop" };
        for (const command of [deleteStaging, deleteShop]) {
          const refused = yield* Effect.exit(writeStoreAsMember(bob, slug, command));
          const error = Exit.isFailure(refused) ? Cause.squash(refused.cause) : null;
          assert.deepInclude(error, { _tag: "Forbidden", message: "Only an owner or admin of this Organization can do this." });
        }

        yield* drizzle.update(member).set({ role: "admin" }).where(eq(member.userId, bobUser));
        assert.isTrue((yield* writeStoreAsMember(bob, slug, deleteStaging)).ok, "an admin deletes the Environment");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, deleteShop)).ok, "an admin deletes the Project");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** Alice owns Project shop, whose production is Protected and runs `web` from acme/web; Bob is a member of her Organization. */
const protectedShop = Effect.fn(function* () {
  const session = yield* signUp("alice");
  const alice = yield* resolveCaller(signedIn(session));
  const bobSession = yield* signUp("bob");
  const bobUser = (yield* resolveCaller(signedIn(bobSession))).userId;
  const slug = alice.organization.slug;
  const { drizzle } = yield* Database;
  yield* drizzle.insert(member).values({ userId: bobUser, organizationId: alice.organization.id, role: "member" });
  yield* drizzle.insert(githubInstallation).values({ installationId: 7, accountLogin: "acme", accountType: "Organization" });
  yield* drizzle.insert(organizationGithubInstallation).values({
    organizationId: alice.organization.id, installationId: 7, connectedByUserId: alice.userId, connectedGithubLogin: "alice",
  });
  yield* drizzle.insert(githubRepositoryCache).values({
    installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
    private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
  });
  const protection = Effect.fn(function* () {
    const listing = yield* callStoreAsMember(alice, slug, { operation: "read", query: { query: "environments", project: "shop" } });
    return listing.ok && listing.value.view === "environments"
      ? listing.value.environments.map((environment) => [environment.name, environment.protected])
      : assert.fail("no environments");
  });
  const role = (value: "admin" | "member") => drizzle.update(member).set({ role: value }).where(eq(member.userId, bobUser));

  assert.isTrue((yield* writeStoreAsMember(alice, slug, shop)).ok);
  assert.deepStrictEqual(yield* protection(), [["production", true]]);
  assert.isTrue((yield* writeStoreAsMember(alice, slug, {
    command: "create_git_service", id: SERVICE, environment: here, name: "web", repository: "acme/web", branch: null,
  })).ok, "an owner connects a repository in a Protected Environment");
  return { alice, session, bob: { userId: bobUser, session: bobSession }, organizationId: alice.organization.id, slug, protection, role };
});
const branch = (value: string): ConfigCommand => ({
  command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.branch", value }],
});
const protect = (on: boolean): ConfigCommand => ({
  command: "set_environment_protected", environment: { project: "shop", environment: "production" }, protected: on,
});
const adminOnly = "Only an owner or admin of this Organization can do this.";
const refusalOf = (result: { ok: true } | { ok: false; refusal: { code: string; message: string } }) =>
  result.ok ? null : [result.refusal.code, result.refusal.message];

it.live(
  "a new Project's production is Protected, and only an owner or admin changes its source or its protection",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, bob, slug, protection, role } = yield* protectedShop();

        assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, branch("dev"))), ["forbidden", adminOnly]);
        const clears: Array<ConfigCommand> = [
          { command: "edit", environment: here, expect: null, changes: [{ op: "unset", path: "web.repository" }] },
          { command: "edit", environment: here, expect: null, changes: [{ op: "patch", path: "web", value: { repository: null } }] },
        ];
        for (const clear of clears) {
          assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, clear)), ["forbidden", adminOnly]);
        }
        const unprotect = yield* Effect.exit(writeStoreAsMember(bob, slug, protect(false)));
        assert.deepInclude(Exit.isFailure(unprotect) ? Cause.squash(unprotect.cause) : null, { _tag: "Forbidden", message: adminOnly });
        assert.deepStrictEqual(yield* protection(), [["production", true]]);

        yield* role("admin");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, branch("dev"))).ok, "an admin changes the branch");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, protect(false))).ok, "an admin unprotects production");
        assert.deepStrictEqual(yield* protection(), [["production", false]]);

        yield* role("member");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, branch("main"))).ok, "a member changes the branch once it isn't Protected");
        const values = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web") });
        assert.deepInclude(values.ok && values.value.view === "environment" ? values.value.values : null, { branch: "main" });
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "the CLI's write skips the dashboard's role check, and a Protected Environment's source and protection still need an owner or admin",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { bob, organizationId, protection, role } = yield* protectedShop();
        const cli = (command: ConfigCommand) =>
          callStore(organizationId, bob.userId, { operation: "write", command }, { source: "cli", agent: null });

        assert.deepStrictEqual(refusalOf(yield* cli(protect(false))), ["forbidden", adminOnly]);
        assert.deepStrictEqual(refusalOf(yield* cli(branch("dev"))), ["forbidden", adminOnly]);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);

        yield* role("admin");
        assert.isTrue((yield* cli(protect(false))).ok, "an admin unprotects production from the CLI");
        assert.deepStrictEqual(yield* protection(), [["production", false]]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a repository a member changes in a Branch syncs into Protected production only for an owner or admin",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, bob, slug, role } = yield* protectedShop();
        const { drizzle } = yield* Database;
        yield* drizzle.insert(githubRepositoryCache).values({
          installationId: 7, repositoryId: 12, name: "docs", fullName: "acme/docs", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/docs", repoUpdatedAt: new Date(),
        });
        const fixWeb = { project: null, environment: "fix-web" };
        const production = { project: null, environment: "production" };
        assert.isTrue((yield* writeStoreAsMember(bob, slug, {
          command: "create_branch", id: "00000000-0000-4000-8000-000000000005", from: production, name: "fix-web",
          copy: ["web"], live: [], setup: [], keep: false,
        })).ok);
        assert.isTrue((yield* writeStoreAsMember(bob, slug, {
          command: "edit", environment: fixWeb, expect: null, changes: [{ op: "set", path: "web.repository", value: "acme/docs" }],
        })).ok, "a member changes the repository in an unprotected Branch");
        const sync = yield* callStoreAsMember(bob, slug, { operation: "read", query: { query: "sync", from: fixWeb, into: production } });
        const version = sync.ok && sync.value.view === "sync" ? sync.value.version : assert.fail("no sync");
        const syncUp: ConfigCommand = { command: "sync", from: fixWeb, into: production, version };
        assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, syncUp)), ["forbidden", adminOnly]);

        yield* role("admin");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, syncUp)).ok, "an admin syncs it");
        const save: ConfigCommand = { command: "publish", environment: production, version: null };
        assert.isTrue((yield* writeStoreAsMember(bob, slug, save)).ok, "an admin saves it");
        const values = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web") });
        assert.deepInclude(values.ok && values.value.view === "environment" ? values.value.values : null, { repository: "acme/docs" });
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** The unprotect approvals filed in `organizationId`. */
const unprotectApprovals = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ id: operationApprovals.id, status: operationApprovals.status }).from(operationApprovals)
    .where(and(eq(operationApprovals.organizationId, organizationId), eq(operationApprovals.command, "unprotect")));
});
const unprotectByAgent = (key: string, approvalId: string | null) => ({
  tool: "environment_protect", input: { protected: false }, key, approvalId,
  page: { at: "here" as const, project: "shop", environment: "production" },
});

it.live(
  "an agent's unprotect waits on one human approval, which holds for that protection only",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, organizationId, slug, protection } = yield* protectedShop();
        const agent = { ...alice, via: "agent" as const };
        /** Asks to unprotect production, and returns the approval it waits on. */
        const ask = Effect.fn(function* (key: string, approvalId: string | null) {
          const asked = yield* runAgentCall(agent, unprotectByAgent(key, approvalId));
          if (!("asking" in asked)) return assert.fail(`no approval asked: ${JSON.stringify(asked)}`);
          assert.strictEqual(asked.asking.refusal.code, "approval_required");
          assert.deepInclude(asked.asking.refusal.details, { operation: { verb: "unprotect", name: "production" } });
          const { approval } = asked.asking.refusal.details as { approval: string };
          return { id: asked.asking.approvalId, digest: approval };
        });
        const approve = Effect.fn(function* (approval: { id: string; digest: string }) {
          assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, approval.id, { approve: { digest: approval.digest } })).ok);
        });

        const first = yield* ask("first", null);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        assert.deepStrictEqual(yield* unprotectApprovals(organizationId), [{ id: first.id, status: "pending" }]);
        yield* approve(first);
        assert.deepInclude(yield* runAgentCall(agent, unprotectByAgent("first", first.id)), {
          outcome: { ok: true, value: { at: "shop/production", protected: false } },
        });
        assert.deepStrictEqual(yield* protection(), [["production", false]]);

        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(true))).ok, "an owner protects it again");
        const again = yield* ask("again", first.id);
        assert.notStrictEqual(again.id, first.id, "the old approval doesn't lift the new protection");
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an unprotect from `ployz mcp` asks again after the Environment is protected again",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, session, slug, protection } = yield* protectedShop();
        const mcp = (approvalId: string | null) => {
          const headers = signedIn(session);
          headers.set("x-ployz-via", "agent");
          if (approvalId !== null) headers.set("x-ployz-approval", approvalId);
          return send("write", headers, protect(false));
        };

        const asked = yield* mcp(null);
        assert.strictEqual(asked.json.error?.code, "approval_required", JSON.stringify(asked.json));
        const approvalId = asked.json.error?.details?.approval_id ?? assert.fail("no approval id");
        const digest = asked.json.error?.details?.approval ?? assert.fail("no approval digest");
        assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, approvalId, { approve: { digest } })).ok);
        assert.strictEqual((yield* mcp(approvalId)).status, 200);
        assert.deepStrictEqual(yield* protection(), [["production", false]]);

        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(true))).ok);
        const again = yield* mcp(approvalId);
        assert.strictEqual(again.json.error?.code, "approval_required", JSON.stringify(again.json));
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a member's agent asking to unprotect is refused, and no human is asked",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, bob, organizationId, slug, protection } = yield* protectedShop();
        const agent = { ...alice, userId: bob.userId, via: "agent" as const };

        assert.deepStrictEqual(yield* runAgentCall(agent, unprotectByAgent("member", null)), {
          outcome: { ok: false, refusal: { code: "forbidden", message: adminOnly, details: null } },
        });
        const { drizzle } = yield* Database;
        yield* drizzle.update(sessionTable).set({ activeOrganizationId: organizationId, activeOrganizationSlug: slug })
          .where(eq(sessionTable.userId, bob.userId));
        const mcp = signedIn(bob.session);
        mcp.set("x-ployz-via", "agent");
        const fromMcp = yield* send("write", mcp, protect(false));
        assert.deepStrictEqual([fromMcp.json.error?.code, fromMcp.json.error?.message], ["forbidden", adminOnly]);
        assert.deepStrictEqual(yield* unprotectApprovals(organizationId), []);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/**
 * Has the agent's owner protect production anew just before the Store sees its unprotect, as another
 * write landing after Cloud decided whether to ask.
 */
const protectedAgainFirst = Effect.fn(function* (organizationId: string) {
  const store = yield* cloudStore;
  const write = store.write.bind(store);
  let landed = false;
  vi.spyOn(store, "write").mockImplementation(async (organization, command, trusted, principal) => {
    if (!landed && command.command === "set_environment_protected" && !command.protected) {
      landed = true;
      const current = await store.read(organizationId, { query: "environments", project: "shop" });
      const protectedNow = current.view === "environments" && current.environments.some((environment) => environment.protected);
      if (protectedNow) await write(organization, protect(false), trusted, null);
      await write(organization, protect(true), trusted, null);
    }
    return write(organization, command, trusted, principal);
  });
  return () => landed;
});
const refusedCode = (answer: Effect.Success<ReturnType<typeof runAgentCall>>) =>
  "outcome" in answer && "refusal" in answer.outcome ? answer.outcome.refusal.code : JSON.stringify(answer);

it.live(
  "an agent's unprotect the protection changed under after Cloud decided is refused, approved or not",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, session, organizationId, slug, protection } = yield* protectedShop();
        const agent = { ...alice, via: "agent" as const };

        // Approved for the first protection; production is unprotected and protected again before the write lands.
        const asked = yield* runAgentCall(agent, unprotectByAgent("raced", null));
        if (!("asking" in asked)) return assert.fail(`no approval asked: ${JSON.stringify(asked)}`);
        const { approval } = asked.asking.refusal.details as { approval: string };
        assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, asked.asking.approvalId, { approve: { digest: approval } })).ok);
        const landed = yield* protectedAgainFirst(organizationId);
        assert.strictEqual(refusedCode(yield* runAgentCall(agent, unprotectByAgent("raced", asked.asking.approvalId))), "conflict");
        assert.isTrue(landed());
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        vi.restoreAllMocks();

        // Unprotected, so no one is asked; a person protects it before the write lands.
        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(false))).ok);
        const unasked = yield* protectedAgainFirst(organizationId);
        assert.strictEqual(refusedCode(yield* runAgentCall(agent, unprotectByAgent("unasked", null))), "conflict");
        assert.isTrue(unasked());
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        vi.restoreAllMocks();

        // The same from `ployz mcp`.
        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(false))).ok);
        const fromMcp = yield* protectedAgainFirst(organizationId);
        const headers = signedIn(session);
        headers.set("x-ployz-via", "agent");
        const mcp = yield* send("write", headers, protect(false));
        assert.strictEqual(mcp.json.error?.code, "conflict", JSON.stringify(mcp.json));
        assert.isTrue(fromMcp());
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        vi.restoreAllMocks();
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

/** Evidence of someone who may protect, and no more. */
const owner = { repositories: [], domains: { cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] }, may_protect: true };
/** Runs `between` once, after Cloud's first Environment listing reads and before it answers. */
const afterListing = Effect.fn(function* (between: () => Promise<void>) {
  const store = yield* cloudStore;
  const read = store.read.bind(store);
  let landed = false;
  vi.spyOn(store, "read").mockImplementation(async (organization, query) => {
    const answer = await read(organization, query);
    if (!landed && query.query === "environments") {
      landed = true;
      await between();
    }
    return answer;
  });
  return () => landed;
});
const asAgent = (session: string, approvalId: string | null) => {
  const headers = signedIn(session);
  headers.set("x-ployz-via", "agent");
  if (approvalId !== null) headers.set("x-ployz-approval", approvalId);
  return headers;
};
/** Creates Environment `name` in `shop`, Protected once, as someone who may protect. */
const protectedEnvironment = Effect.fn(function* (organizationId: string, id: string, name: string) {
  const store = yield* cloudStore;
  return async () => {
    await store.write(organizationId, { command: "create_environment", id, project: "shop", name }, owner);
    await store.write(organizationId, { command: "set_environment_protected", environment: { project: "shop", environment: name }, protected: true }, owner);
  };
});

it.live(
  "an agent's approved unprotect of the default is refused when another Environment became the default before it lands",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, session, organizationId, slug } = yield* protectedShop();
        const store = yield* cloudStore;
        // staging is Protected once too, so its protection reads the same as production's.
        yield* Effect.promise(yield* protectedEnvironment(organizationId, "00000000-0000-4000-8000-000000000090", "staging"));
        const agent = { ...alice, via: "agent" as const };
        const listing = Effect.fn(function* () {
          const environments = yield* Effect.promise(() => store.read(organizationId, { query: "environments", project: "shop" }));
          return environments.view === "environments"
            ? environments.environments.map((environment) => [environment.name, environment.default, environment.protected])
            : assert.fail("no environments");
        });
        const toDefault = (environment: string) => async () => {
          await store.write(organizationId, { command: "set_default_environment", environment: { project: "shop", environment } }, owner);
        };

        for (const surface of ["sidebar", "mcp"] as const) {
          // The human approves unprotecting the default, production.
          yield* Effect.promise(toDefault("production"));
          const call = { tool: "environment_protect", input: { protected: false }, key: `default-${surface}`, approvalId: null, page: { at: "organization" as const } };
          const asked = yield* runAgentCall(agent, call);
          if (!("asking" in asked)) return assert.fail(`no approval asked: ${JSON.stringify(asked)}`);
          assert.deepInclude(asked.asking.refusal.details, { operation: { verb: "unprotect", name: "production" } });
          const { approval } = asked.asking.refusal.details as { approval: string };
          assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, asked.asking.approvalId, { approve: { digest: approval } })).ok);

          const moved = yield* afterListing(toDefault("staging"));
          const code = surface === "sidebar"
            ? refusedCode(yield* runAgentCall(agent, { ...call, approvalId: asked.asking.approvalId }))
            : (yield* send("write", asAgent(session, asked.asking.approvalId), { command: "set_environment_protected", environment: here, protected: false })).json.error?.code;
          assert.strictEqual(code, "conflict", surface);
          assert.isTrue(moved());
          vi.restoreAllMocks();
          assert.deepStrictEqual(yield* listing(), [["production", false, true], ["staging", true, true]], surface);
        }
        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(true))).ok);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an agent's unprotect of an Environment Cloud can't find is refused, even when it's created before the write lands",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, session, organizationId, protection } = yield* protectedShop();
        const agent = { ...alice, via: "agent" as const };
        const legs = [
          ["sidebar", "staging", "00000000-0000-4000-8000-000000000091", (environment: string) =>
            runAgentCall(agent, { ...unprotectByAgent("absent", null), page: { at: "here" as const, project: "shop", environment } }).pipe(Effect.map(refusedCode))],
          ["mcp", "qa", "00000000-0000-4000-8000-000000000092", (environment: string) =>
            send("write", asAgent(session, null), { command: "set_environment_protected", environment: { project: "shop", environment }, protected: false })
              .pipe(Effect.map((answer) => answer.json.error?.code))],
        ] as const;
        for (const [surface, name, id, unprotect] of legs) {
          const created = yield* afterListing(yield* protectedEnvironment(organizationId, id, name));
          assert.strictEqual(yield* unprotect(name), "not_found", surface);
          assert.isTrue(created());
          vi.restoreAllMocks();
          assert.deepInclude(yield* protection(), [name, true], surface);
        }
        const { drizzle } = yield* Database;
        assert.deepStrictEqual(yield* drizzle.select().from(operationApprovals).where(eq(operationApprovals.organizationId, organizationId)), []);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "someone no longer a member may not protect, and Cloud's own write still may",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { bob, organizationId, protection, role } = yield* protectedShop();
        yield* role("admin");
        const { drizzle } = yield* Database;
        yield* drizzle.delete(member).where(eq(member.userId, bob.userId));
        const gone = yield* callStore(organizationId, bob.userId, { operation: "write", command: protect(false) }, { source: "cli", agent: null });
        assert.deepStrictEqual(refusalOf(gone), ["forbidden", adminOnly]);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);

        assert.isTrue((yield* callStore(organizationId, null, { operation: "write", command: protect(false) })).ok, "Cloud itself unprotects");
        assert.deepStrictEqual(yield* protection(), [["production", false]]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a member's repository change in a Protected Environment is refused before Cloud asks GitHub about it",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { bob, slug } = yield* protectedShop();
        const asked = () => github.calls.filter((call) => call.path.startsWith("/repos/acme/down")).length;
        const before = asked();
        const down: ConfigCommand = {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.repository", value: "acme/down" }],
        };
        assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, down)), ["forbidden", adminOnly]);
        assert.strictEqual(asked(), before, "GitHub was never asked");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "Store writes reach Cloud's change log once committed, and a refused write logs nothing",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const { drizzle } = yield* Database;
        let seen = 0;
        /** The Store changes logged since the last call, as `table: keys`; each statement logs once. */
        const logged = Effect.fn(function* () {
          const rows = yield* drizzle.execute<{ seq: string; entry: string }>(sql`
            select seq::text, source_table || ': ' || array_to_string(changed_ids, ',') as entry
            from organization_change
            where source_table like 'config_%' and seq > ${seen}
              and organization_id = (select organization_id::uuid from config_project limit 1)
            order by seq
          `, "objects");
          seen = Math.max(seen, ...rows.map((row) => Number(row.seq)));
          return [...new Set(rows.map((row) => row.entry))];
        });

        assert.strictEqual((yield* request("write", alice, shop)).status, 200);
        assert.deepStrictEqual(yield* logged(), [`config_project: ${PROJECT}`, `config_environment: ${ENVIRONMENT}`]);
        assert.strictEqual((yield* request("write", alice, web)).status, 200);
        assert.sameMembers(yield* logged(), [`config_environment: ${ENVIRONMENT}`, `config_node_introduction: ${ENVIRONMENT}`]);

        // The edit locks and rewrites the Environment before refusing the value, and rolls it all back.
        const refused = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.replicas", value: "many" }],
        });
        assert.strictEqual(refused.json.error?.code, "invalid_argument");
        assert.deepStrictEqual(yield* logged(), []);

        assert.strictEqual((yield* request("write", alice, { command: "publish", environment: here, version: null, accept_volume_loss: [] })).status, 200);
        // Every write locks its Environment's row, which logs the Environment too.
        assert.sameMembers(yield* logged(), [`config_environment: ${ENVIRONMENT}`, `config_saved: ${ENVIRONMENT}`]);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "an admission over HTTPS hands its Deployment to Cloud's worker, once",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      const sent: unknown[] = [];
      let down = false;
      vi.spyOn(inngest, "send").mockImplementation(async (event) => {
        if (down) throw new Error("Inngest is down");
        sent.push(event);
        return { ids: [] };
      });
      const layer = yield* cloudLayer({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(signedIn(alice))).organization.id);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const admit: ConfigCommand = {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null, accept_volume_loss: [],
        };
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        // A retried request replays the admission and sends the same event, which Inngest drops.
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        const event = {
          id: "config-deployment-admitted-00000000-0000-4000-8000-000000000101",
          name: "config/deployment.admitted",
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: admit.id, trigger: "user" },
        };
        expect(sent).toEqual([event, event]);

        // Inngest down: the admission stands and says so; the sweep, or Deploy now, hands it over later.
        down = true;
        const stranded = yield* request("write", alice, { ...admit, id: "00000000-0000-4000-8000-000000000102" });
        assert.strictEqual(stranded.status, 200);
        assert.deepInclude(stranded.json, { written: "deployment" });

        // Deploy now hands the stranded admission to the worker again, unkeyed; a refused start sends nothing.
        down = false;
        const start: ConfigCommand = { command: "start", deployment: "00000000-0000-4000-8000-000000000102" };
        assert.strictEqual((yield* request("write", alice, start)).status, 200);
        expect(sent.at(-1)).toEqual({
          name: "config/deployment.admitted",
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: start.deployment, trigger: "user" },
        });
        const superseded = yield* request("write", alice, { ...start, deployment: admit.id });
        assert.strictEqual(superseded.status, 409);
        assert.strictEqual(sent.length, 3);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Deploy meets the copies it takes before the Store sees it, on every path to the Store",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const organizationId = (yield* resolveCaller(signedIn(alice))).organization.id;
        yield* enrollStoreServer(organizationId);
        yield* request("write", alice, shop);
        const VOLUME = "00000000-0000-4000-8000-000000000004";
        yield* request("write", alice, { command: "create_volume", id: VOLUME, environment: here, name: "data", storage: { kind: "docker" }, mounts: [] });
        const { drizzle } = yield* Database;
        const web1 = ids.machineId("1");
        const web2 = ids.machineId("2");
        yield* drizzle.insert(volumePlacement).values({
          volumeId: VOLUME, organizationId, environmentId: ENVIRONMENT, mirrors: [web2], staged: { primary: web1, mirrors: [], deletes: [] },
        });
        // No Server answers here, so the copy check fails closed; the Store never admits the Deploy.
        const admitted = yield* request("write", alice, {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null, accept_volume_loss: [],
        });
        assert.strictEqual(admitted.status, 503);
        assert.strictEqual(admitted.json.error?.code, "unavailable");
        assert.strictEqual((yield* request("read", alice, { query: "deployment", id: "00000000-0000-4000-8000-000000000101" })).status, 404);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "removing the last Server lets Cloud go of its Cluster, and a Deploy after it asks for a Server",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const organizationId = (yield* resolveCaller(signedIn(alice))).organization.id;
        yield* enrollStoreServer(organizationId);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        // Cloud's Cluster has this Server and `other`, which leaves first without a reset. It resets this one when
        // asked; the first reset doesn't finish.
        const machineId = ids.machineId("0");
        const other = ids.machineId("1");
        const { drizzle } = yield* Database;
        const encryption = yield* SecretEncryption;
        const [enrolled] = yield* drizzle.select().from(organizationMachine);
        if (!enrolled) return assert.fail("no Server enrolled");
        yield* drizzle.insert(organizationMachine).values({
          ...enrolled, machineId: other, isDialEntry: false, encryptedCapability: encryption.encrypt("ployz1:other"),
        });
        const warnings: Array<string | null> = ["Docker reset failed: busy volume", null];
        // No Server answers the runtime watch, so their capabilities are unknown and every removal still goes out.
        const connected = asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.fail(new RuntimeConnectionFailure({ cause: "timed out" })),
          removeMachine: () => Effect.sync(() => ({ reset_warning: warnings.shift() ?? null })),
          removeMachineMembership: () => Effect.void,
        });
        const runtime = {
          cancel: () => Effect.void,
          open: () => Effect.succeed({ status: "connected" as const, connected }),
        };
        const userId = (yield* resolveCaller(signedIn(alice))).userId;
        let runs = 0;
        const removeServer = (server: MachineId, noReset: boolean) => Effect.gen(function* () {
          const removal = { organizationId, machineId: server, confirmDataLoss: [], noReset };
          const attempt = yield* requestMachineRemoveAttempt({ ...removal, requestedByUserId: userId });
          const inngestRunId = `run-${++runs}`;
          yield* prepareMachineRemoveAttemptActivity({ attemptId: attempt.id, inngestRunId, now: new Date() });
          const removed = yield* Effect.scoped(removeMachineActivity({ ...removal, id: attempt.id, inngestRunId }));
          assert.strictEqual(removed.kind, "removed");
          if (removed.kind !== "removed") return assert.fail("not removed");
          const release = yield* releaseServerActivity({ ...removal, ...removed });
          yield* completeMachineRemoveAttemptActivity({
            attemptId: attempt.id,
            inngestRunId,
            completion: { state: "succeeded", result: { resetWarning: removed.resetWarning, release } },
            now: new Date(),
          });
          return { removed, release };
        }).pipe(Effect.provideService(OrganizationRuntime, runtime));
        const remove = removeServer(machineId, false);

        // Taken out without a reset: Cloud drops its row, and another Server keeps the Cluster.
        assert.deepStrictEqual((yield* removeServer(other, true)).release, { kind: "others_remain" });
        assert.deepStrictEqual((yield* drizzle.select().from(organizationMachine)).map((row) => row.machineId), [machineId]);

        // A reset that didn't finish is partial: the Server may keep Cloud's key, so Cloud keeps its hold.
        const partial = yield* remove;
        assert.strictEqual(partial.release.kind, "kept");
        assert.strictEqual((yield* drizzle.select().from(organizationPairing)).length, 1);
        assert.strictEqual((yield* drizzle.select().from(organizationMachine)).length, 1);

        const done = yield* remove;
        assert.deepStrictEqual(done.release, { kind: "released" });
        // Cloud holds nothing of it any more: no pairing, no Server, no device key.
        assert.deepStrictEqual(yield* drizzle.select().from(organizationMachine), []);
        assert.deepStrictEqual(yield* drizzle.select().from(organizationPairing), []);

        const refused = yield* request("write", alice, {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null, accept_volume_loss: [],
        });
        assert.strictEqual(refused.status, 409);
        assert.strictEqual(refused.json.error?.code, "conflict");
        assert.strictEqual(refused.json.error?.details?.next, "ployz server add");

        // A replay of that release after a new Cluster was paired, even through a Server of the same ID, touches nothing.
        yield* drizzle.insert(organizationPairing).values({
          organizationId, encryptedPairingSecret: encryption.encrypt("ppair_replacement"),
          founderPublicKey: "founder-key", founderClaimMachineId: ids.machineId("1"),
        });
        yield* drizzle.insert(organizationMachine).values({
          ...enrolled, clusterKey: createHash("sha256").update("ppair_replacement").digest("hex"),
        });
        const replay = yield* releaseServerActivity({ organizationId, machineId, generation: done.removed.generation, resetWarning: null, noReset: false });
        assert.strictEqual(replay.kind, "kept");
        assert.strictEqual((yield* drizzle.select().from(organizationPairing)).length, 1);
        assert.strictEqual((yield* drizzle.select().from(organizationMachine)).length, 1);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a removal retried after a provider failure sends the same removal again, and a run that lost the attempt sends none",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const caller = yield* resolveCaller(signedIn(alice));
        const organizationId = caller.organization.id;
        yield* enrollStoreServer(organizationId);
        const machineId = ids.machineId("0");
        const { drizzle } = yield* Database;
        const sent: Array<{ machine: string; confirmed: readonly unknown[] }> = [];
        const replies = [
          Effect.fail(new PloyzProviderError({ operation: "remove machine", cause: "reply lost" })),
          Effect.succeed({ reset_warning: null }),
        ];
        const connected = asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.fail(new RuntimeConnectionFailure({ cause: "timed out" })),
          removeMachine: (machine: string, confirm: { confirmed: readonly unknown[] }) =>
            Effect.suspend(() => {
              sent.push({ machine, confirmed: confirm.confirmed });
              return replies.shift() ?? Effect.die("asked too often");
            }),
        });
        const runtime = {
          cancel: () => Effect.void,
          open: () => Effect.succeed({ status: "connected" as const, connected }),
        };
        const removal = { organizationId, machineId, confirmDataLoss: [], noReset: false };
        const attempt = yield* requestMachineRemoveAttempt({ ...removal, requestedByUserId: caller.userId });
        const inngestRunId = "run-retry";
        yield* prepareMachineRemoveAttemptActivity({ attemptId: attempt.id, inngestRunId, now: new Date() });
        const remove = (run: string) => Effect.scoped(removeMachineActivity({ ...removal, id: attempt.id, inngestRunId: run }))
          .pipe(Effect.provideService(OrganizationRuntime, runtime));

        // The first step fails at the provider; Inngest retries the step, which asks the same way again.
        const failed = yield* Effect.exit(remove(inngestRunId));
        assert.isTrue(Exit.isFailure(failed));
        const [asked] = yield* drizzle.select().from(machineRemoveAttempt).where(eq(machineRemoveAttempt.id, attempt.id));
        const firstAsk = asked?.removeSentAt ?? assert.fail("no ask stamped");
        const removed = yield* remove(inngestRunId);
        assert.deepStrictEqual(sent, [
          { machine: machineId, confirmed: [] },
          { machine: machineId, confirmed: [] },
        ]);
        if (removed.kind !== "removed") return assert.fail(`not removed: ${removed.kind}`);
        assert.isNull(removed.resetWarning);
        const [retried] = yield* drizzle.select().from(machineRemoveAttempt).where(eq(machineRemoveAttempt.id, attempt.id));
        assert.deepStrictEqual(retried?.removeSentAt, firstAsk);

        // A run that no longer owns the attempt asks nothing.
        const superseded = yield* remove("run-stale");
        assert.strictEqual(superseded.kind, "superseded");
        assert.strictEqual(sent.length, 2);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Server that reset before its Forget failed is forgotten without another removal, and keeps the Leave's warning",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const caller = yield* resolveCaller(signedIn(alice));
        const organizationId = caller.organization.id;
        yield* enrollStoreServer(organizationId);
        const machineId = ids.machineId("0");
        const resetWarning = "Docker reset failed";
        const sent: string[] = [];
        const forgets = [
          Effect.fail(new PloyzProviderError({ operation: "remove machine membership", cause: { code: "unavailable", message: "suspect" } })),
          Effect.void,
        ];
        const connected = asTestDouble<PloyzSession>()({
          watchFirstFrame: () => Effect.fail(new RuntimeConnectionFailure({ cause: "timed out" })),
          removeMachine: () =>
            Effect.suspend(() => {
              sent.push("remove");
              return Effect.fail(new LeftNotForgotten({ resetWarning, cause: "forget failed" }));
            }),
          removeMachineMembership: () =>
            Effect.suspend(() => {
              sent.push("forget");
              return forgets.shift() ?? Effect.die("asked too often");
            }),
        });
        const runtime = {
          cancel: () => Effect.void,
          open: () => Effect.succeed({ status: "connected" as const, connected }),
        };
        const removal = { organizationId, machineId, confirmDataLoss: [], noReset: false };
        const attempt = yield* requestMachineRemoveAttempt({ ...removal, requestedByUserId: caller.userId });
        const inngestRunId = "run-left";
        yield* prepareMachineRemoveAttemptActivity({ attemptId: attempt.id, inngestRunId, now: new Date() });
        const step = (left: { resetWarning: string | null } | null) =>
          Effect.scoped(removeMachineActivity({ ...removal, id: attempt.id, inngestRunId }, left))
            .pipe(Effect.provideService(OrganizationRuntime, runtime));

        const first = yield* step(null);
        if (first.kind !== "left") return assert.fail(`not left: ${first.kind}`);
        assert.strictEqual(first.resetWarning, resetWarning);
        // The Forget step retries while the Server still reads as suspect, and never sends the removal again.
        assert.isTrue(Exit.isFailure(yield* Effect.exit(step(first))));
        const removed = yield* step(first);
        assert.deepStrictEqual(sent, ["remove", "forget", "forget"]);
        if (removed.kind !== "removed") return assert.fail(`not removed: ${removed.kind}`);
        assert.strictEqual(removed.resetWarning, resetWarning);
        const release = yield* releaseServerActivity({ ...removal, ...removed });
        assert.strictEqual(release.kind, "kept");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Git-backed Service connects only repositories and branches Cloud checked for the Organization",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        assert.strictEqual((yield* request("write", alice, shop)).status, 200);
        // Alice installed the GitHub App on acme with acme/web; her Organization reads it through that installation.
        const database = yield* Database;
        const [row] = yield* database.drizzle.select({ id: user.id, organization: member.organizationId }).from(user)
          .innerJoin(member, eq(member.userId, user.id)).where(eq(user.email, "alice@example.test"));
        if (!row) assert.fail("no user");
        const userId = row.id;
        yield* database.drizzle.insert(githubInstallation).values({ installationId: 7, accountLogin: "acme", accountType: "Organization" });
        yield* database.drizzle.insert(organizationGithubInstallation).values({
          organizationId: row.organization, installationId: 7, connectedByUserId: userId, connectedGithubLogin: "alice",
        });
        yield* database.drizzle.insert(githubRepositoryCache).values({
          installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
          private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
        });
        let services = 10;
        const git = (name: string, repository: string, branch?: string): ConfigCommand => ({
          command: "create_git_service", id: `00000000-0000-4000-8000-0000000000${(services += 1)}`,
          environment: here, name, repository, branch: branch ?? null,
        });

        const created = yield* request("write", alice, git("web", "Acme/Web", "dev"));
        assert.strictEqual(created.status, 200);
        assert.include(created.json.staged ?? [], "web.branch");
        assert.deepInclude(github.calls, { path: "/repos/acme/web/git/ref/heads%2Fdev", access: { type: "organization", organizationId: row.organization, installationId: 7 } });
        const web = yield* request("read", alice, get("web"));
        assert.deepInclude(web.json.values, { repository: "acme/web", branch: "dev" });

        const secret = yield* request("write", alice, git("docs", "acme/secret"));
        assert.strictEqual(secret.status, 404);
        assert.strictEqual(secret.json.error?.details?.next, "ployz github connect");
        const gone = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.branch", value: "gone" }],
        });
        assert.strictEqual(gone.status, 404);
        assert.strictEqual(gone.json.error?.details?.next, "ployz github ls acme/web");

        // A public repository needs no installation; acme/docs has no dev branch, so web moves to its default.
        const moved = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.repository", value: "acme/docs" }],
        });
        assert.strictEqual(moved.status, 200);
        assert.deepInclude((yield* request("read", alice, get("web"))).json.values, { repository: "acme/docs", branch: "main" });

        const forged = yield* request("write", alice, { command: "create_git_service", installation_id: 7 });
        assert.strictEqual(forged.status, 422);
        const down = yield* request("write", alice, git("api", "acme/down"));
        assert.strictEqual(down.status, 503);
        assert.strictEqual(down.json.error?.code, "unavailable");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "domains: a custom one is free, and a generated one reserves the Cluster Domain when it is first admitted",
  () =>
    Effect.gen(function* () {
      const hostedDns = yield* Effect.acquireRelease(Effect.promise(startFakeHostedDns), (fake) => Effect.promise(() => fake.close()));
      const inngest = new Inngest({ id: "config-store-test" });
      const sent: Array<{ readonly name: string }> = [];
      vi.spyOn(inngest, "send").mockImplementation(async (event) => {
        sent.push(...[event].flat());
        return { ids: [] };
      });
      const hosted: PolarService = {
        mode: "hosted",
        productId: "pro",
        listActiveSubscriptions: () => Effect.die("not used"),
        createCustomerPortal: () => Effect.die("not used"),
      };
      const layer = yield* cloudLayer({ polar: hosted, hostedDnsUrl: hostedDns.url }, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(signedIn(alice))).organization.id);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const custom: ConfigCommand = { command: "add_domain", environment: here, service: "web", hostname: "app.example.com", port: null };
        assert.strictEqual((yield* request("write", alice, custom)).status, 200);

        const generated = yield* request("write", alice, { ...custom, hostname: null });
        // No Cluster Domain yet: only admission reserves one.
        assert.deepInclude(generated.json.domain, { kind: "generated", prefix: "web", hostname: null });
        assert.lengthOf(hostedDns.requests, 0);
        const listed = yield* request("read", alice, { query: "domains", environment: here, service: null });
        assert.deepStrictEqual(listed.json.domains?.map((domain) => [domain.status, domain.action?.type]), [
          ["setting_up", "deploy"],
          ["setting_up", "deploy"],
        ]);

        const admit: ConfigCommand = {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null,
          accept_volume_loss: [],
        };
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        assert.lengthOf(hostedDns.requests.filter((call) => call.method === "POST"), 1);
        const deploying = yield* request("read", alice, { query: "domains", environment: here, service: null });
        const hostname = deploying.json.domains?.[1]?.hostname ?? "";
        assert.match(hostname, /^web\.[a-z0-9-]+\.ployz\.test$/);

        const caller = yield* resolveCaller(signedIn(alice));
        const listQuery: ConfigQuery = { query: "domains", environment: here, service: null };
        const edited = yield* writeStoreAsMember(caller, caller.organization.slug, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.replicas", value: 2 }],
        }, [listQuery]);
        const carried = edited.ok ? edited.answers?.[0]?.result : undefined;
        const answered = carried?.ok && carried.value.view === "domains" ? carried.value.domains : [];
        assert.deepStrictEqual(answered.map((domain) => domain.hostname), ["app.example.com", hostname],
          "a write carries the domains view as the view's own read shows it, generated hostname included");

        // A check asks for a Cluster Domain sync before it reads the domain afresh.
        sent.length = 0;
        const checked = yield* request("read", alice, { query: "domain", environment: here, domain: "web" });
        assert.strictEqual(checked.json.domain?.hostname, hostname);
        assert.deepStrictEqual(sent.map((event) => event.name), ["cluster-domain/sync.requested"]);
      }).pipe(Effect.provide(layer));
    }).pipe(Effect.scoped),
  60_000,
);

it.live(
  "a committed write answers as committed when rereading a shown view fails, and leaves that view to the tab",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const caller = yield* resolveCaller(signedIn(alice));
        // Reading domains loads the Cluster Domain from Cloud's database; make that read fail after the write commits.
        const { drizzle } = yield* Database;
        yield* drizzle.execute(sql`ALTER TABLE organization_cluster_domain RENAME TO organization_cluster_domain_gone`);
        const domains: ConfigQuery = { query: "domains", environment: here, service: null };
        const edited = yield* writeStoreAsMember(caller, caller.organization.slug, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.replicas", value: 2 }],
        }, [domains, get("web.replicas")]);
        yield* drizzle.execute(sql`ALTER TABLE organization_cluster_domain_gone RENAME TO organization_cluster_domain`);

        assert.isTrue(edited.ok, "the write committed, so it answers as committed");
        assert.deepStrictEqual(edited.ok ? edited.answers?.map((answer) => answer.query) : null, [get("web.replicas")],
          "only the views that reread answer; the tab refetches the domains view");
        const read = yield* request("read", alice, get("web.replicas"));
        assert.strictEqual(read.json.settings?.[0]?.value, 2);
      }).pipe(Effect.provide(layer));
    }).pipe(Effect.scoped),
  60_000,
);

/** A gzipped tar of a source directory, as `ployz deploy --upload` sends it: everything under `source/`. */
const sourceArchive = Effect.promise(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "upload-test-"));
  await mkdir(path.join(root, "source"));
  await writeFile(path.join(root, "source", "Dockerfile"), "FROM scratch\n");
  const chunks: Buffer[] = [];
  for await (const chunk of tar.c({ gzip: true, cwd: root, portable: true }, ["source"])) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks);
});

const upload = Effect.fn(function* (deploymentId: string, session: string, body: Buffer) {
  const headers = signedIn(session);
  headers.set("content-type", "application/gzip");
  const response = yield* handleConfigRequest(new Request(`${origin}/api/config/upload/${deploymentId}`, {
    method: "POST", headers, body: new Blob([new Uint8Array(body)]),
  }));
  return { status: response.status, json: decodeReply(yield* Effect.promise(() => response.text())) };
});

it.live(
  "an upload belongs to the one Deployment it was sent for, names its uploader, and goes once that Deployment ends",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const layer = yield* cloudLayer({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(signedIn(alice))).organization.id);
        const bob = yield* signUp("bob");
        yield* request("write", alice, shop);
        yield* request("write", alice, {
          command: "create_service", id: SERVICE, environment: here, name: "app", image: null,
        });
        const [first, second] = ["00000000-0000-4000-8000-000000000201", "00000000-0000-4000-8000-000000000202"];
        const archive = yield* sourceArchive;
        assert.strictEqual((yield* upload(first, alice, archive)).status, 200);
        assert.strictEqual((yield* upload(second, alice, archive)).status, 200);
        // Written once, and never shared: not again, and not by another Organization.
        const again = yield* upload(first, alice, archive);
        assert.strictEqual(again.status, 409);
        assert.strictEqual(again.json.error?.code, "conflict");
        assert.strictEqual((yield* upload(first, bob, archive)).status, 409);
        const { id: organizationId } = (yield* resolveCaller(signedIn(alice))).organization;
        const store = yield* cloudStore;
        const tooBig = yield* receiveUpload(store, organizationId, "00000000-0000-4000-8000-000000000203",
          new Blob([archive]).stream(), archive.length - 1);
        assert.strictEqual(tooBig?.code, "invalid_argument");

        // Cloud, not the caller, names who uploaded it.
        const admitted = yield* request("write", alice, {
          command: "admit", admit: "deploy", id: first, environment: here, services: [], version: null, accept_volume_loss: [],
          upload: { digest: "d".repeat(64), base: null, uploader: "mallory" },
        });
        assert.strictEqual(admitted.status, 200);
        const view = yield* Effect.promise(() => store.read(organizationId, { query: "deployment", id: first }));
        expect(view).toMatchObject({ upload: { digest: "d".repeat(64), uploader: "alice" } });
        assert.strictEqual((yield* upload(first, alice, archive)).status, 409);

        const { drizzle } = yield* Database;
        const held = (deploymentId: string) => drizzle.select().from(uploadChunk).where(eq(uploadChunk.deploymentId, deploymentId))
          .pipe(Effect.map((rows) => rows.length));
        // Kept while it may still run, so a replaced worker finds it.
        yield* releaseUpload(store, organizationId, first);
        assert.strictEqual(yield* held(first), 1);
        // Its Server left: its upload was read, and nothing ran.
        yield* drizzle.delete(organizationMachine);
        const ran = yield* runStoreDeployment({ organizationId, environmentId: ENVIRONMENT, deploymentId: first, trigger: "user" }, "cloud-test");
        expect(ran).toMatchObject({ ran: { id: first, status: "failed" } });
        assert.strictEqual(yield* held(first), 0);
        assert.strictEqual(yield* held(second), 1);
      }).pipe(Effect.provide(layer));
    }).pipe(Effect.scoped),
  60_000,
);

it.live(
  "a user's write carries what they hold: none named is a defect, and callStoreBy sends the role's set",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const organizationId = "00000000-0000-4000-8000-00000000c001";
        const owner = yield* seedStoreOrganization(organizationId);
        const { drizzle } = yield* Database;
        const [linus] = yield* drizzle.insert(user).values({ email: "linus@example.test", name: "Linus" }).returning();
        const memberId = linus?.id ?? assert.fail("no member");
        yield* drizzle.insert(member).values({ userId: memberId, organizationId, role: "member" });
        const organization = { id: organizationId, slug: "shop" };
        const by = (userId: string, role: "owner" | "member"): Access => ({ kind: "member", userId, role, organization });
        const buildOrder = write({ command: "set_build_order", build_order: "servers-then-github" });

        // A user's write with no permissions named is Cloud's defect, never a grant.
        const unnamed = yield* Effect.exit(callStore(organizationId, owner, buildOrder));
        assert.isTrue(Exit.isFailure(unnamed), String(unnamed));
        if (Exit.isFailure(unnamed)) assert.match(String(Cause.squash(unnamed.cause)), /names no permissions/);
        // The Store refuses a member what their role lacks, and lands an owner's.
        const refused = yield* callStoreBy(organizationId, by(memberId, "member"), buildOrder);
        assert.isFalse(refused.ok);
        if (!refused.ok) assert.deepInclude(refused.refusal, { code: "forbidden", message: "Needs org:settings" });
        assert.isTrue((yield* callStoreBy(organizationId, by(owner, "owner"), buildOrder)).ok);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
