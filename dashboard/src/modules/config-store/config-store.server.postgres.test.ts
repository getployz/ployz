import { assert, it } from "@effect/vitest";
import { expect, vi } from "vitest";
import { type ConfigCommand, type ConfigQuery, type MachineId, RpcError } from "@ployz/sdk";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { and, eq, sql } from "drizzle-orm";
import * as tar from "tar";
import { Cause, Effect, Exit, Schema } from "effect";
import { Inngest } from "inngest";
import type { PolarService } from "#/modules/billing/polar-provider.server";
import { startFakeHostedDns } from "#/modules/cluster-domain/hosted-dns.test-fixture";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import { runAgentCall } from "#/modules/agent/agent-runner.server";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { operationApprovals } from "#/modules/approvals/tables";
import { callStore, callStoreAsMember, callStoreBy, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { type Access, requireAccess, SupportStaff, supportStaffOf } from "#/modules/organization/access.server";
import type { Caller } from "#/modules/identity/actor";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { permissionsOf } from "#/modules/organization/permissions";
import * as ids from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloudWith } from "#/test/store-cloud";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { runStoreDeployment } from "#/modules/config-store/store-deployment.server";
import { uploadChunk } from "#/modules/config-store/tables";
import { extractUpload, receiveUpload, releaseUploads, UPLOAD_LIMIT, uploadOwner } from "#/modules/config-store/upload.server";
import { resolveCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken, knownEnvironment, mintOrganizationTokenUnchecked } from "#/modules/identity/organization-token.server";
import type { TokenGrant } from "#/modules/identity/token-grant";
import { Auth, AuthLive } from "#/server/auth.server";
import { githubInstallation, githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import { member, session as sessionTable, user } from "#/modules/identity/tables";
import { machineRemoveAttempt, organizationMachine, serverAccess } from "#/modules/machines/tables";
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
const cloudServices = Effect.fn(function* (
  overrides: { readonly polar?: PolarService; readonly hostedDnsUrl?: string } = {},
  inngest = new Inngest({ id: "config-store-test" }),
) {
  return yield* storeTestCloudWith(AuthLive, {
    github: github.service,
    inngest,
    // No Hosted DNS answers unless a test starts one.
    env: { PLOYZ_HOSTED_DNS_URL: overrides.hostedDnsUrl ?? "http://127.0.0.1:9/" },
    polar: overrides.polar ?? { mode: "self_hosted" },
  });
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
      revision: Schema.optionalKey(Schema.Number), next: Schema.optionalKey(Schema.String), max_bytes: Schema.optionalKey(Schema.Number),
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
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a write over the body cap is refused with the cap, and nothing is staged",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        assert.strictEqual((yield* request("write", alice, shop)).status, 200);
        assert.strictEqual((yield* request("write", alice, web)).status, 200);

        const huge = yield* request("write", alice, {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.env.HUGE", value: "Z".repeat(4 * 1024 * 1024) }],
        });
        assert.strictEqual(huge.status, 422);
        assert.strictEqual(huge.json.error?.code, "invalid_argument");
        assert.strictEqual(huge.json.error?.message, "The request is over 4 MiB.");
        assert.deepStrictEqual(huge.json.error?.details, { max_bytes: 4 * 1024 * 1024 });
        assert.deepStrictEqual((yield* request("read", alice, get("web.env.HUGE"))).json.settings ?? [], []);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a browser's cookie signs no config request in, so a page on another site can't write through it",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const alice = yield* signUpBoth("alice");
        const forged = new Headers({ cookie: alice.cookie, "sec-fetch-site": "cross-site", origin: "https://evil.example" });
        assert.strictEqual((yield* send("write", forged, shop)).status, 401);
        assert.strictEqual((yield* request("read", alice.session, get(null))).status, 404);
        assert.strictEqual((yield* send("write", new Headers({ cookie: alice.cookie, "sec-fetch-site": "same-origin" }), shop)).status, 401);
        assert.strictEqual((yield* request("write", alice.session, shop)).status, 200);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "the dashboard reaches the Store as a member of the Organization it names, refusals intact",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a member stages but can't delete an Environment or a Project or set the Build Order; an admin can",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
        const buildOrder: ConfigCommand = { command: "set_build_order", build_order: "servers-only" };
        for (const command of [deleteStaging, deleteShop, buildOrder]) {
          const refused = yield* Effect.exit(writeStoreAsMember(bob, slug, command));
          const error = Exit.isFailure(refused) ? Cause.squash(refused.cause) : null;
          assert.deepInclude(error, { _tag: "Forbidden", message: "Only an owner or admin of this Organization can do this." });
        }

        yield* drizzle.update(member).set({ role: "admin" }).where(eq(member.userId, bobUser));
        assert.isTrue((yield* writeStoreAsMember(bob, slug, deleteStaging)).ok, "an admin deletes the Environment");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, deleteShop)).ok, "an admin deletes the Project");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, buildOrder)).ok, "an admin sets the Build Order");
      }).pipe(Effect.provide(cloud));
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
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "only an owner or admin makes a Deploy token for a Protected Environment",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { session, bob, organizationId, slug, role } = yield* protectedShop();
        yield* activeIn(bob.userId, organizationId, slug);
        const production = { preset: "deploy", environment: "production" } as const;
        const minted = (session: string) => Effect.gen(function* () {
          const exit = yield* Effect.exit(knownEnvironment(yield* resolveCaller(signedIn(session)), production));
          return Exit.isSuccess(exit) ? "minted" : Cause.squash(exit.cause);
        });

        assert.deepInclude(yield* minted(bob.session), {
          _tag: "Validation", message: "production is Protected; only an owner or admin can make a Deploy token for it.",
        });
        assert.strictEqual(yield* minted(session), "minted");
        yield* role("admin");
        assert.strictEqual(yield* minted(bob.session), "minted");
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "the CLI's write skips the dashboard's role check, a Protected Environment's source and protection still need an owner or admin, and its unprotect waits on a human",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, bob, organizationId, slug, protection, role } = yield* protectedShop();
        yield* activeIn(bob.userId, organizationId, slug);
        const write = (command: ConfigCommand, approvalId?: string) => {
          const headers = signedIn(bob.session);
          if (approvalId !== undefined) headers.set("x-ployz-approval", approvalId);
          return send("write", headers, command);
        };
        const cli = (command: ConfigCommand) =>
          write(command).pipe(Effect.map(({ status, json }) => [status, json.error?.code, json.error?.message]));

        assert.deepStrictEqual(yield* cli(protect(false)), [403, "forbidden", adminOnly]);
        assert.deepStrictEqual(yield* cli(branch("dev")), [403, "forbidden", adminOnly]);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);

        yield* role("admin");
        const asked = yield* write(protect(false));
        assert.strictEqual(asked.json.error?.code, "approval_required", "an admin's unprotect from the CLI waits on a human");
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        const approvalId = asked.json.error?.details?.approval_id ?? assert.fail("no approval id");
        const digest = asked.json.error?.details?.approval ?? assert.fail("no approval digest");
        assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, approvalId, { approve: { digest } })).ok);
        assert.strictEqual((yield* write(protect(false), approvalId)).status, 200, "an admin unprotects production from the CLI once approved");
        assert.deepStrictEqual(yield* protection(), [["production", false]]);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a repository a member changes in a Branch syncs into Protected production only for an owner or admin",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
        assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, syncUp)), ["forbidden", "Needs environments:protect"]);

        yield* role("admin");
        assert.isTrue((yield* writeStoreAsMember(bob, slug, syncUp)).ok, "an admin syncs it");
        const save: ConfigCommand = { command: "publish", environment: production, version: null };
        assert.isTrue((yield* writeStoreAsMember(bob, slug, save)).ok, "an admin saves it");
        const values = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web") });
        assert.deepInclude(values.ok && values.value.view === "environment" ? values.value.values : null, { repository: "acme/docs" });
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

/** Makes `organizationId` the active Organization of every session of user `userId`, as the CLI's login does. */
const activeIn = Effect.fn(function* (userId: string, organizationId: string, slug: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(sessionTable).set({ activeOrganizationId: organizationId, activeOrganizationSlug: slug })
    .where(eq(sessionTable.userId, userId));
});

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
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, organizationId, slug, protection } = yield* protectedShop();
        const agent = { ...alice, via: "agent" as const };
        /** Asks to unprotect production, and returns the approval it waits on. */
        const ask = Effect.fn(function* (key: string, approvalId: string | null) {
          const asked = yield* runAgentCall(agent, unprotectByAgent(key, approvalId));
          if (!("asking" in asked)) return assert.fail(`no approval asked: ${JSON.stringify(asked)}`);
          assert.strictEqual(asked.asking.refusal.code, "approval_required");
          expect(asked.asking.refusal.details).toMatchObject({ operation: { verb: "unprotect", name: "shop/production" } });
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "an unprotect from `ployz mcp` asks again after the Environment is protected again",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a member's agent asking to unprotect is refused, and no human is asked",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, bob, organizationId, slug, protection } = yield* protectedShop();
        const agent = { ...alice, userId: bob.userId, role: "member" as const, via: "agent" as const };

        assert.deepStrictEqual(yield* runAgentCall(agent, unprotectByAgent("member", null)), {
          outcome: { ok: false, refusal: { code: "forbidden", message: adminOnly, details: null } },
        });
        yield* activeIn(bob.userId, organizationId, slug);
        const mcp = signedIn(bob.session);
        mcp.set("x-ployz-via", "agent");
        const fromMcp = yield* send("write", mcp, protect(false));
        assert.deepStrictEqual([fromMcp.json.error?.code, fromMcp.json.error?.message], ["forbidden", adminOnly]);
        assert.deepStrictEqual(yield* unprotectApprovals(organizationId), []);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

/** Ployz staff with support access to Alice's Organization: not a member, acting as the dashboard's support Caller. */
const supportIn = Effect.fn(function* (alice: { readonly userId: string }, organizationId: string, slug: string) {
  const { drizzle } = yield* Database;
  const session = yield* signUp("staff");
  const staff = (yield* resolveCaller(signedIn(session))).userId;
  yield* drizzle.insert(organizationSupportAccess).values({ organizationId, allowed: true, changedByUserId: alice.userId });
  const access = yield* requireAccess({ userId: staff }, slug, "org:read").pipe(Effect.provideService(SupportStaff, supportStaffOf([staff])));
  if (access.kind !== "support") return assert.fail(`staff isn't support: ${access.kind}`);
  const caller: Caller = {
    userId: staff, organization: access.organization, credential: { kind: "session", id: "support" },
    role: null, support: access.shows, via: "dashboard",
  };
  return { caller, session };
});

it.live(
  "support doesn't change Protected production's source or protection, or remove its Project",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, organizationId, slug, protection } = yield* protectedShop();
        const { caller: staff } = yield* supportIn(alice, organizationId, slug);
        const projects = Effect.fn(function* () {
          const listing = yield* callStoreAsMember(alice, slug, { operation: "read", query: { query: "projects" } });
          return listing.ok && listing.value.view === "projects" ? listing.value.projects.map((project) => project.name) : assert.fail("no projects");
        });

        yield* Effect.gen(function* () {
          assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(staff, slug, branch("dev"))), ["forbidden", adminOnly]);
          const unprotect = yield* Effect.exit(writeStoreAsMember(staff, slug, protect(false)));
          assert.deepInclude(Exit.isFailure(unprotect) ? Cause.squash(unprotect.cause) : null, { _tag: "Forbidden", message: adminOnly });
          const removal = yield* writeStoreAsMember(staff, slug, { command: "remove", project: "shop" });
          assert.deepStrictEqual(refusalOf(removal), ["forbidden", "Needs environments:protect"]);
        }).pipe(Effect.provideService(SupportStaff, supportStaffOf([staff.userId])));

        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        assert.deepStrictEqual(yield* projects(), ["shop"]);
        const values = yield* callStoreAsMember(alice, slug, { operation: "read", query: get("web") });
        assert.deepInclude(values.ok && values.value.view === "environment" ? values.value.values : null, { branch: "main" });
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "support's agent asking to unprotect is refused, and no human is asked",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, organizationId, slug, protection } = yield* protectedShop();
        const { caller: staff, session } = yield* supportIn(alice, organizationId, slug);

        const fromSidebar = yield* runAgentCall({ ...staff, via: "agent" }, unprotectByAgent("support", null))
          .pipe(Effect.provideService(SupportStaff, supportStaffOf([staff.userId])));
        assert.deepStrictEqual(fromSidebar, { outcome: { ok: false, refusal: { code: "forbidden", message: adminOnly, details: null } } });
        // `ployz mcp` acts only as a member, so support's session is refused before the Store is asked.
        yield* activeIn(staff.userId, organizationId, slug);
        const mcp = signedIn(session);
        mcp.set("x-ployz-via", "agent");
        const fromMcp = yield* send("write", mcp, protect(false));
        assert.strictEqual(fromMcp.status, 403);
        assert.deepStrictEqual(yield* unprotectApprovals(organizationId), []);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);
      }).pipe(Effect.provide(cloud));
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
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

/** Evidence of someone who may protect, and no more. */
const owner = { repositories: [], domains: { cluster_domain: null, certificates: null, ingress_addresses: [], lookups: [] } };
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
      const cloud = yield* cloudServices();
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
          expect(asked.asking.refusal.details).toMatchObject({ operation: { verb: "unprotect", name: "shop/production" } });
          const { approval } = asked.asking.refusal.details as { approval: string };
          assert.isTrue((yield* decideApproval({ ...alice, via: "dashboard" }, asked.asking.approvalId, { approve: { digest: approval } })).ok);

          const moved = yield* afterListing(toDefault("staging"));
          const code = surface === "sidebar"
            ? refusedCode(yield* runAgentCall(agent, { ...call, approvalId: asked.asking.approvalId }))
            : (yield* send("write", asAgent(session, asked.asking.approvalId), { command: "set_environment_protected", environment: here, protected: false })).json.error?.code;
          // The sidebar's tool unprotects the Environment it reviewed, which moved; `ployz mcp`'s write names the default,
          // now staging, which the production approval doesn't cover.
          assert.strictEqual(code, surface === "sidebar" ? "conflict" : "forbidden", surface);
          assert.isTrue(moved());
          vi.restoreAllMocks();
          assert.deepStrictEqual(yield* listing(), [["production", false, true], ["staging", true, true]], surface);
        }
        assert.isTrue((yield* writeStoreAsMember(alice, slug, protect(true))).ok);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "an agent's unprotect of an Environment Cloud can't find is refused, even when it's created before the write lands",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "an agent's unprotect is refused with the listing's own refusal when Cloud can't list Environments, and writes nothing",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { alice, session, organizationId, protection } = yield* protectedShop();
        const store = yield* cloudStore;
        const read = store.read.bind(store);
        const details = { next: "retry the listing" };
        vi.spyOn(store, "read").mockImplementation(async (organization, query) => {
          if (query.query === "environments") throw new RpcError({ code: "unavailable", message: "Environment listing refused", details });
          return read(organization, query);
        });
        const writes = vi.spyOn(store, "write");

        const sidebar = yield* runAgentCall({ ...alice, via: "agent" }, unprotectByAgent("refused-listing", null));
        assert.deepStrictEqual(sidebar, { outcome: { ok: false, refusal: { code: "unavailable", message: "Environment listing refused", details } } });
        const mcp = yield* send("write", asAgent(session, null), protect(false));
        assert.strictEqual(mcp.status, 503);
        assert.deepStrictEqual(
          [mcp.json.error?.code, mcp.json.error?.message, mcp.json.error?.details],
          ["unavailable", "Environment listing refused", details],
        );
        assert.strictEqual(writes.mock.calls.length, 0);
        vi.restoreAllMocks();

        assert.deepStrictEqual(yield* protection(), [["production", true]]);
        const { drizzle } = yield* Database;
        assert.deepStrictEqual(yield* drizzle.select().from(operationApprovals).where(eq(operationApprovals.organizationId, organizationId)), []);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "someone no longer a member may not protect, and Cloud's own write still may",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { bob, organizationId, slug, protection, role } = yield* protectedShop();
        yield* role("admin");
        yield* activeIn(bob.userId, organizationId, slug);
        const { drizzle } = yield* Database;
        yield* drizzle.delete(member).where(eq(member.userId, bob.userId));
        assert.strictEqual((yield* request("write", bob.session, protect(false))).status, 403);
        assert.deepStrictEqual(yield* protection(), [["production", true]]);

        assert.isTrue((yield* callStore(organizationId, null, { operation: "write", command: protect(false) })).ok, "Cloud itself unprotects");
        assert.deepStrictEqual(yield* protection(), [["production", false]]);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a member's repository change in a Protected Environment is refused before Cloud asks GitHub about it",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { bob, slug } = yield* protectedShop();
        const asked = () => github.calls.filter((call) => call.path.startsWith("/repos/acme/down")).length;
        const before = asked();
        const down: ConfigCommand = {
          command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.repository", value: "acme/down" }],
        };
        assert.deepStrictEqual(refusalOf(yield* writeStoreAsMember(bob, slug, down)), ["forbidden", adminOnly]);
        assert.strictEqual(asked(), before, "GitHub was never asked");
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "Store writes reach Cloud's change log once committed, and a refused write logs nothing",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
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
      const cloud = yield* cloudServices({}, inngest);
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
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: admit.id, number: 1, trigger: "user" },
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
          data: { organizationId: expect.any(String), environmentId: ENVIRONMENT, deploymentId: start.deployment, number: 2, trigger: "user" },
        });
        const superseded = yield* request("write", alice, { ...start, deployment: admit.id });
        assert.strictEqual(superseded.status, 409);
        assert.strictEqual(sent.length, 3);

        // Its last Server left: Deploy now refuses as a Deploy does, and hands nothing to the worker.
        const { drizzle } = yield* Database;
        yield* drizzle.delete(organizationMachine);
        const serverless = yield* request("write", alice, start);
        assert.strictEqual(serverless.status, 409);
        expect(serverless.json).toMatchObject({
          error: { code: "conflict", details: { next: "ployz server add" } },
        });
        assert.strictEqual(sent.length, 3);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a Sync that closes its Branch asks its Organization's Store to sweep now",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      const sent: unknown[] = [];
      vi.spyOn(inngest, "send").mockImplementation(async (event) => {
        sent.push(event);
        return { ids: [] };
      });
      const cloud = yield* cloudServices({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const organizationId = (yield* resolveCaller(signedIn(alice))).organization.id;
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const fixWeb = { project: null, environment: "fix-web" };
        const production = { project: null, environment: "production" };
        assert.strictEqual((yield* request("write", alice, {
          command: "create_branch", id: "00000000-0000-4000-8000-000000000005", from: production, name: "fix-web",
          copy: ["web"], live: [], setup: [], keep: false,
        })).status, 200);
        assert.strictEqual((yield* request("write", alice, {
          command: "edit", environment: fixWeb, expect: null, changes: [{ op: "set", path: "web.image", value: "web:2" }],
        })).status, 200);
        const sweep = { name: "config/sweep.requested", data: { organizationId } };
        const read = yield* callStore(organizationId, null, { operation: "read", query: { query: "sync", from: fixWeb, into: production } });
        const version = read.ok && read.value.view === "sync" ? read.value.version : assert.fail("no sync");
        const sync: ConfigCommand = { command: "sync", from: fixWeb, into: production, version, close_after: true };
        assert.deepInclude((yield* request("write", alice, sync)).json, { written: "synced" });
        expect(sent).toContainEqual(sweep);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a Deploy meets the copies it takes before the Store sees it, on every path to the Store",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "removing the last Server lets Cloud go of its Cluster, and a Deploy after it asks for a Server",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
          const removal = { organizationId, machineId: server, serverName: null, confirmDataLoss: [], noReset };
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a removal retried after a provider failure sends the same removal again, and a run that lost the attempt sends none",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
        const removal = { organizationId, machineId, serverName: null, confirmDataLoss: [], noReset: false };
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a Server that reset before its Forget failed is forgotten without another removal, and keeps the Leave's warning",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
        const removal = { organizationId, machineId, serverName: null, confirmDataLoss: [], noReset: false };
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

/**
 * Removes Cloud's dial entry `machine-0` while `machine-1` is listed beside it. Dialing `machine-1` reaches it only when
 * `otherAnswers`, and the Runtime Watch sends a frame only when `frameAnswers`; entering through `machine-0` refuses
 * its own removal as the Engine does.
 */
const removeDialEntry = Effect.fn(function* (otherAnswers: boolean, frameAnswers = true) {
  const alice = yield* signUp("alice");
  const caller = yield* resolveCaller(signedIn(alice));
  const organizationId = caller.organization.id;
  yield* enrollStoreServer(organizationId);
  const machineId = ids.machineId("0");
  const other = ids.machineId("1");
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const [enrolled] = yield* drizzle.select().from(organizationMachine);
  if (!enrolled) return assert.fail("no Server enrolled");
  yield* drizzle.insert(organizationMachine).values({
    ...enrolled, machineId: other, isDialEntry: false, encryptedCapability: encryption.encrypt("ployz1:other"),
  });
  const frame = {
    machines: [
      { machine: { id: machineId, name: "machine-0", runtime: {} }, membership: "up" },
      { machine: { id: other, name: "machine-1", runtime: {} }, membership: "up" },
    ],
    containers: [],
  };
  const sent: string[] = [];
  const through = (entry: MachineId) => asTestDouble<PloyzSession>()({
    watchFirstFrame: () => frameAnswers ? Effect.succeed(frame) : Effect.fail(new RuntimeConnectionFailure({ cause: "timed out" })),
    removeMachine: (target: string) => Effect.suspend(() => {
      sent.push(`${target} through ${entry}`);
      return target === entry
        ? Effect.fail(new PloyzProviderError({
          operation: "remove machine",
          cause: { code: "invalid_argument", message: "the current entry Machine cannot be removed while another Machine is visible" },
        }))
        : Effect.succeed({ reset_warning: null });
    }),
  });
  const runtime = {
    cancel: () => Effect.void,
    open: (_organizationId: string, entry?: { readonly only: MachineId } | { readonly avoiding: MachineId }) =>
      Effect.succeed(entry !== undefined && "avoiding" in entry && entry.avoiding === machineId
        ? otherAnswers
          ? { status: "connected" as const, connected: through(other) }
          : { status: "unreachable" as const, error: null }
        : { status: "connected" as const, connected: through(machineId) }),
  };
  const removal = { organizationId, machineId, serverName: null, confirmDataLoss: [], noReset: false };
  const attempt = yield* requestMachineRemoveAttempt({ ...removal, requestedByUserId: caller.userId });
  const inngestRunId = "run-dial-entry";
  yield* prepareMachineRemoveAttemptActivity({ attemptId: attempt.id, inngestRunId, now: new Date() });
  const removed = yield* Effect.exit(Effect.scoped(removeMachineActivity({ ...removal, id: attempt.id, inngestRunId }))
    .pipe(Effect.provideService(OrganizationRuntime, runtime)));
  return { removed, sent, machineId, other };
});

it.live(
  "removing Cloud's dial entry while another Server is listed enters through the other Server",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { removed, sent, machineId, other } = yield* removeDialEntry(true);
        assert.deepStrictEqual(sent, [`${machineId} through ${other}`]);
        if (!Exit.isSuccess(removed)) return assert.fail(`removal failed: ${Cause.pretty(removed.cause)}`);
        assert.strictEqual(removed.value.kind, "removed");
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a Server Cloud reaches only through itself refuses its removal at once while another is listed, sending nothing",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { removed, sent } = yield* removeDialEntry(false);
        assert.deepStrictEqual(sent, []);
        if (!Exit.isSuccess(removed)) return assert.fail(`removal failed: ${Cause.pretty(removed.cause)}`);
        if (removed.value.kind !== "refused") return assert.fail(`not refused: ${removed.value.kind}`);
        assert.strictEqual(removed.value.failureCode, "conflict");
        assert.match(removed.value.message, /machine-0 can't carry its own removal/);
        assert.match(removed.value.message, /machine-1/);
        assert.notMatch(removed.value.message, /forget machine-0/i);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "without a Cluster frame, a Server Cloud reaches only through itself still refuses its removal at once while another is paired",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const { removed, sent } = yield* removeDialEntry(false, false);
        assert.deepStrictEqual(sent, []);
        if (!Exit.isSuccess(removed)) return assert.fail(`removal failed: ${Cause.pretty(removed.cause)}`);
        if (removed.value.kind !== "refused") return assert.fail(`not refused: ${removed.value.kind}`);
        assert.strictEqual(removed.value.failureCode, "conflict");
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

/**
 * A retried removal whose Server the Cluster already forgot: the SDK answers removed with this warning (ployz
 * `RESET_UNCONFIRMED`), and the Server keeps one device key. `others` Servers of the same pairing stay enrolled.
 */
const removeForgottenServer = Effect.fn(function* (others: readonly MachineId[]) {
  const alice = yield* signUp("alice");
  const caller = yield* resolveCaller(signedIn(alice));
  const organizationId = caller.organization.id;
  yield* enrollStoreServer(organizationId);
  const machineId = ids.machineId("0");
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const [enrolled] = yield* drizzle.select().from(organizationMachine);
  if (!enrolled) return assert.fail("no Server enrolled");
  for (const other of others) {
    yield* drizzle.insert(organizationMachine).values({
      ...enrolled, machineId: other, isDialEntry: false, encryptedCapability: encryption.encrypt(`ployz1:${other}`),
    });
  }
  yield* drizzle.insert(serverAccess).values({
    organizationId, machineId, credentialId: crypto.randomUUID(), credentialKind: "session", userId: caller.userId,
    encryptedCapability: encryption.encrypt("ployz1:cli"),
  });
  const connected = asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.fail(new RuntimeConnectionFailure({ cause: "timed out" })),
    removeMachine: () => Effect.succeed({ reset_warning: "its reset couldn't be confirmed: it was already gone from the Cluster" }),
  });
  const runtime = { cancel: () => Effect.void, open: () => Effect.succeed({ status: "connected" as const, connected }) };
  const removal = { organizationId, machineId, serverName: null, confirmDataLoss: [], noReset: false };
  const attempt = yield* requestMachineRemoveAttempt({ ...removal, requestedByUserId: caller.userId });
  const inngestRunId = "run-forgotten";
  yield* prepareMachineRemoveAttemptActivity({ attemptId: attempt.id, inngestRunId, now: new Date() });
  const removed = yield* Effect.scoped(removeMachineActivity({ ...removal, id: attempt.id, inngestRunId }))
    .pipe(Effect.provideService(OrganizationRuntime, runtime));
  if (removed.kind !== "removed") return assert.fail(`not removed: ${removed.kind}`);
  const release = yield* releaseServerActivity({ ...removal, ...removed });
  yield* completeMachineRemoveAttemptActivity({
    attemptId: attempt.id, inngestRunId, now: new Date(),
    completion: { state: "succeeded", result: { resetWarning: removed.resetWarning, release } },
  });
  const [settled] = yield* drizzle.select().from(machineRemoveAttempt).where(eq(machineRemoveAttempt.id, attempt.id));
  return {
    settled,
    servers: (yield* drizzle.select().from(organizationMachine)).map((row) => row.machineId).sort(),
    keys: (yield* drizzle.select().from(serverAccess)).map((row) => row.machineId),
    pairings: (yield* drizzle.select().from(organizationPairing)).length,
  };
});

it.live(
  "a Server already gone from the Cluster loses Cloud's row and its device keys while other Servers keep the Cluster",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      const other = ids.machineId("1");
      const after = yield* removeForgottenServer([other]).pipe(Effect.provide(cloud));
      assert.strictEqual(after.settled?.state, "succeeded");
      assert.deepStrictEqual(after.settled?.result, {
        resetWarning: "its reset couldn't be confirmed: it was already gone from the Cluster",
        release: { kind: "others_remain" },
      });
      assert.deepStrictEqual(after.servers, [other]);
      assert.deepStrictEqual(after.keys, []);
      assert.strictEqual(after.pairings, 1);
    }),
  60_000,
);

it.live(
  "the last Server already gone from the Cluster keeps Cloud's hold, as its reset couldn't be confirmed",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      const after = yield* removeForgottenServer([]).pipe(Effect.provide(cloud));
      assert.strictEqual(after.settled?.state, "succeeded");
      assert.strictEqual(after.settled?.result?.release.kind, "kept");
      assert.deepStrictEqual(after.servers, [ids.machineId("0")]);
      assert.deepStrictEqual(after.keys, [ids.machineId("0")]);
      assert.strictEqual(after.pairings, 1);
    }),
  60_000,
);

it.live(
  "a Git-backed Service connects only repositories and branches Cloud checked for the Organization",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "domains: a custom one is free, and a generated one reserves the Cluster Domain when it is first staged",
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
      const cloud = yield* cloudServices({ polar: hosted, hostedDnsUrl: hostedDns.url }, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(signedIn(alice))).organization.id);
        yield* request("write", alice, shop);
        yield* request("write", alice, web);
        const custom: ConfigCommand = { command: "add_domain", environment: here, service: "web", hostname: "app.example.com", port: null };
        assert.strictEqual((yield* request("write", alice, custom)).status, 200);

        const reservations = () => hostedDns.requests.filter((call) => call.method === "POST").length;
        const reserved = () => Effect.gen(function* () {
          const { drizzle } = yield* Database;
          return yield* drizzle.execute<{ name: string }>(sql`select name from organization_cluster_domain`, "objects");
        });
        const planned = yield* request("read", alice, { query: "plan", environment: here, services: [] });
        assert.strictEqual(planned.status, 200);
        assert.strictEqual(reservations(), 0, "a plan reads the Cluster Domain and reserves none");
        assert.lengthOf(yield* reserved(), 0);

        const generate: ConfigCommand = { ...custom, hostname: null };
        hostedDns.state.failWith = 503;
        const failed = yield* request("write", alice, generate);
        hostedDns.state.failWith = null;
        assert.strictEqual(failed.status, 503);
        assert.deepStrictEqual(failed.json, {
          error: { code: "unavailable", message: "Cloud couldn't reserve the Cluster Domain; try again.", details: null },
        });
        assert.lengthOf(yield* reserved(), 0, "a failed reserve keeps no Cluster Domain");

        hostedDns.requests.length = 0;
        hostedDns.state.reservesTogether = 2;
        const staged = yield* Effect.all([request("write", alice, generate), request("write", alice, generate)], { concurrency: "unbounded" });
        hostedDns.state.reservesTogether = 1;
        const hostname = staged[0].json.domain?.hostname ?? "";
        assert.match(hostname, /^web\.[a-z0-9-]+\.ployz\.test$/, "staging a generated domain names it in full");
        assert.strictEqual(staged[1].json.domain?.hostname, hostname, "two stages at once share one Cluster Domain");
        assert.lengthOf(yield* reserved(), 1);
        const { drizzle } = yield* Database;
        const [kept] = yield* drizzle.select().from(organizationClusterDomain);
        assert.isDefined(kept);
        const keptToken = (yield* SecretEncryption).decrypt(kept.encryptedToken);
        // The fake grants token-N for the Nth request it records.
        const granted = hostedDns.requests.flatMap((call, index) => call.method === "POST" ? [`token-${index + 1}`] : []);
        assert.lengthOf(granted, 2, "both stages reached Hosted DNS");
        assert.deepStrictEqual(
          hostedDns.requests.filter((call) => call.method === "DELETE").map((call) => [call.path, call.authorization]),
          granted.filter((token) => token !== keptToken).map((token) => [`/domains/${kept.name}`, `Bearer ${token}`]),
          "the losing stage releases the name it was granted, and only that one",
        );
        const listed = yield* request("read", alice, { query: "domains", environment: here, service: null });
        assert.deepStrictEqual(listed.json.domains?.map((domain) => [domain.hostname, domain.status, domain.action?.type]), [
          ["app.example.com", "setting_up", "deploy"],
          [hostname, "setting_up", "deploy"],
        ]);
        for (const query of [{ query: "diff", environment: here }, { query: "service", environment: here, service: "web" }] as const) {
          const shown = JSON.stringify((yield* request("read", alice, query)).json);
          assert.include(shown, hostname, `${query.query} names the staged domain in full`);
          assert.notInclude(shown, "<cluster domain>");
        }

        const before = reservations();
        assert.strictEqual((yield* request("write", alice, { command: "remove_domain", environment: here, domain: "web" })).status, 200);
        assert.strictEqual((yield* request("write", alice, generate)).json.domain?.hostname, hostname);
        assert.strictEqual(reservations(), before, "re-staging reserves nothing more");

        // Before anything deployed, only Cloud's evidence knows the Cluster Domain a Branch's copies land under.
        const { drizzle: db } = yield* Database;
        const long = `${"a".repeat(63)}.${"b".repeat(63)}.${"c".repeat(63)}.${"d".repeat(53)}`;
        const reservedName = hostname.slice("web.".length);
        yield* db.update(organizationClusterDomain).set({ name: long });
        const branched = yield* request("write", alice, {
          command: "create_branch", id: "00000000-0000-4000-8000-000000000105", from: { project: null, environment: "production" },
          name: "fixit", copy: ["web"], live: [], setup: [], keep: false,
        });
        yield* db.update(organizationClusterDomain).set({ name: reservedName });
        assert.strictEqual(branched.json.error?.code, "invalid_argument");
        assert.include(branched.json.error?.message, "over a hostname's 253: choose a shorter branch name");

        const admit: ConfigCommand = {
          command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-000000000101", environment: here, services: [], version: null,
          accept_volume_loss: [],
        };
        assert.strictEqual((yield* request("write", alice, admit)).status, 200);
        assert.strictEqual(reservations(), before, "admitting under a reserved Cluster Domain reserves nothing more");

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
      }).pipe(Effect.provide(cloud));
    }).pipe(Effect.scoped),
  60_000,
);

it.live(
  "a committed write answers as committed when rereading a shown view fails, and leaves that view to the tab",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
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
      }).pipe(Effect.provide(cloud));
    }).pipe(Effect.scoped),
  60_000,
);

/** A gzipped tar of a source directory, as `ployz deploy --upload` sends it: everything under `source/`. */
const archiveOf = (dockerfile: string) => Effect.promise(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "upload-test-"));
  await mkdir(path.join(root, "source"));
  await writeFile(path.join(root, "source", "Dockerfile"), dockerfile);
  const chunks: Buffer[] = [];
  for await (const chunk of tar.c({ gzip: true, cwd: root, portable: true }, ["source"])) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks);
});

const sourceArchive = archiveOf("FROM scratch\n");

const upload = Effect.fn(function* (deploymentId: string, session: string, body: Buffer) {
  const headers = signedIn(session);
  headers.set("content-type", "application/gzip");
  const response = yield* handleConfigRequest(new Request(`${origin}/api/config/upload/${deploymentId}`, {
    method: "POST", headers, body: new Blob([new Uint8Array(body)]),
  }));
  return { status: response.status, json: decodeReply(yield* Effect.promise(() => response.text())) };
});

it.live(
  "an upload belongs to the one Deployment it was sent for, names its uploader, and stays until a newer one for its Service or its Service goes",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const cloud = yield* cloudServices({}, inngest);
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
        // Under Cloudflare's 100 MB request cap, so an oversized upload meets Cloud's refusal, not a proxy's error page.
        assert.strictEqual(UPLOAD_LIMIT, 95 * 1024 * 1024);
        const piece = new Uint8Array(1024 * 1024);
        let sent = 0;
        const overCap = new ReadableStream<Uint8Array>({
          pull: (controller) => {
            if (sent > UPLOAD_LIMIT) return controller.close();
            sent += piece.length;
            controller.enqueue(piece);
          },
        });
        const overCloudflare = yield* receiveUpload(store, organizationId, "00000000-0000-4000-8000-000000000204", overCap);
        assert.strictEqual(overCloudflare?.code, "invalid_argument");
        assert.strictEqual(overCloudflare?.message, "The upload is over the 95 MiB compressed source limit.");

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
        const run = (deploymentId: string, environmentId = ENVIRONMENT) =>
          runStoreDeployment({ organizationId, environmentId, deploymentId, trigger: "user" }, "cloud-test");
        const servers = yield* drizzle.select().from(organizationMachine);
        // Admitted while its Server is there, run once it left, so nothing runs.
        const withServers = <A, E, R>(effect: Effect.Effect<A, E, R>) => Effect.gen(function* () {
          yield* drizzle.insert(organizationMachine).values(servers);
          const result = yield* effect;
          yield* drizzle.delete(organizationMachine);
          return result;
        });
        const admit = (
          id: string, environment: { project: null; environment: string | null }, upload: { digest: string; base: null; uploader: null } | null,
          services: string[] = [],
        ) => withServers(request("write", alice, {
          command: "admit", admit: "deploy", id, environment, services, version: null, accept_volume_loss: [], upload,
        }));
        // Kept while it may still run, so a replaced worker finds it.
        yield* releaseUploads(store, organizationId);
        assert.strictEqual(yield* held(first), 1);
        // Its Server left: its upload was read, and nothing ran.
        yield* drizzle.delete(organizationMachine);
        expect(yield* run(first)).toMatchObject({ ran: { id: first, status: "failed" } });
        // Ended, it stays: its Environment's newest Deployment builds from it. The upload not admitted yet stays too.
        assert.strictEqual(yield* held(first), 1);
        assert.strictEqual(yield* held(second), 1);

        // A Deployment that only changes settings builds from the kept upload, and keeps it.
        const settingsOnly = "00000000-0000-4000-8000-000000000205";
        assert.strictEqual((yield* admit(settingsOnly, here, null)).status, 200);
        assert.strictEqual(yield* uploadOwner(store, organizationId, settingsOnly), first);
        yield* run(settingsOnly);
        assert.strictEqual(yield* held(first), 1);

        // A newer upload replaces it.
        assert.strictEqual((yield* admit(second, here, { digest: "e".repeat(64), base: null, uploader: null })).status, 200);
        yield* run(second);
        assert.strictEqual(yield* held(first), 0);
        assert.strictEqual(yield* held(second), 1);

        // Once no Service builds from an upload, it goes.
        yield* request("write", alice, { command: "remove_service", environment: here, service: "app" });
        const withoutApp = "00000000-0000-4000-8000-000000000206";
        assert.strictEqual((yield* admit(withoutApp, here, null)).status, 200);
        assert.strictEqual(yield* uploadOwner(store, organizationId, withoutApp), undefined);
        yield* run(withoutApp);
        assert.strictEqual(yield* held(second), 0);

        // An Environment's upload goes with the Environment: the Store's purge deletes its row, and its Deployments
        // with it. Like an upload never admitted, it goes once a day has passed since it arrived.
        const STAGING = "00000000-0000-4000-8000-000000000207";
        const staging = { project: null, environment: "staging" };
        yield* request("write", alice, { command: "create_environment", id: STAGING, project: "shop", name: "staging" });
        yield* request("write", alice, {
          command: "create_service", id: "00000000-0000-4000-8000-000000000208", environment: staging, name: "app", image: null,
        });
        const third = "00000000-0000-4000-8000-000000000209";
        assert.strictEqual((yield* upload(third, alice, archive)).status, 200);
        assert.strictEqual((yield* admit(third, staging, { digest: "f".repeat(64), base: null, uploader: null })).status, 200);
        yield* run(third, STAGING);
        yield* releaseUploads(store, organizationId);
        assert.strictEqual(yield* held(third), 1);
        yield* drizzle.execute(sql`DELETE FROM config_environment WHERE id = ${STAGING}`);
        yield* drizzle.update(uploadChunk).set({ createdAt: sql`now() - interval '2 days'` }).where(eq(uploadChunk.deploymentId, third));
        yield* releaseUploads(store, organizationId);
        assert.strictEqual(yield* held(third), 0);

        // Each Service keeps its own latest upload: one for api alone leaves web's.
        for (const [id, name] of [["00000000-0000-4000-8000-000000000210", "api"], ["00000000-0000-4000-8000-000000000211", "web"]] as const) {
          yield* request("write", alice, { command: "create_service", id, environment: here, name, image: null });
        }
        const deployment = (n: number) => `00000000-0000-4000-8000-000000000${n}`;
        const [both, apiOnly, webOnly, together, retried] = [deployment(212), deployment(213), deployment(214), deployment(215), deployment(216)];
        const digest = (c: string) => ({ digest: c.repeat(64), base: null, uploader: null });
        assert.strictEqual((yield* upload(both, alice, archive)).status, 200);
        assert.strictEqual((yield* admit(both, here, digest("1"))).status, 200);
        yield* run(both);
        assert.strictEqual((yield* upload(apiOnly, alice, archive)).status, 200);
        assert.strictEqual((yield* admit(apiOnly, here, digest("2"), ["api"])).status, 200);
        yield* run(apiOnly);
        yield* releaseUploads(store, organizationId);
        assert.deepStrictEqual([yield* held(both), yield* held(apiOnly)], [1, 1]);
        // A settings-only Deploy of web builds from web's upload.
        assert.strictEqual((yield* admit(webOnly, here, null, ["web"])).status, 200);
        assert.strictEqual(yield* uploadOwner(store, organizationId, webOnly), both);
        yield* run(webOnly);
        // A Deploy of both builds each from its own upload. Cloud extracts the newer one sent, api's, and keeps both.
        assert.strictEqual((yield* admit(together, here, null)).status, 200);
        assert.strictEqual(yield* uploadOwner(store, organizationId, together), apiOnly);
        yield* run(together);
        yield* releaseUploads(store, organizationId);
        assert.deepStrictEqual([yield* held(both), yield* held(apiOnly)], [1, 1]);
        // A retry of an older Deployment ships its upload, and api's latest upload stays.
        const retry = (id: string, of: string) => withServers(request("write", alice, {
          command: "admit", admit: "retry", id, deployment: of,
        }));
        assert.strictEqual((yield* retry(retried, both)).status, 200);
        yield* run(retried);
        yield* releaseUploads(store, organizationId);
        assert.deepStrictEqual([yield* held(both), yield* held(apiOnly)], [1, 1]);

        // An upload sent again after a failed one stays when the failed one is retried after it: it's api's newest
        // upload. The failed one's went when the newer one replaced it, so its retry needs a new upload, as before.
        const [firstSent, failed, sentAgain, retriedFailed] = [deployment(217), deployment(218), deployment(219), deployment(220)];
        for (const [id, c] of [[firstSent, "3"], [failed, "4"], [sentAgain, "3"]] as const) {
          assert.strictEqual((yield* upload(id, alice, archive)).status, 200);
          assert.strictEqual((yield* admit(id, here, digest(c), ["api"])).status, 200);
          yield* run(id);
        }
        assert.strictEqual((yield* retry(retriedFailed, failed)).status, 200);
        yield* run(retriedFailed);
        yield* releaseUploads(store, organizationId);
        assert.deepStrictEqual([yield* held(failed), yield* held(sentAgain)], [0, 1]);
        // The same content sent twice is kept once: the newer copy.
        assert.strictEqual(yield* held(firstSent), 0);
        assert.strictEqual(yield* held(apiOnly), 0);

        // Release takes no lock, so it may delete an upload as a run reads it, before or mid-read. Either way the
        // run finds no upload, not a broken one: its Services reuse their images or need a new upload.
        const raced = deployment(221);
        assert.strictEqual((yield* upload(raced, alice, archive)).status, 200);
        const select = drizzle.select.bind(drizzle);
        let selects = 0;
        const midRead = vi.spyOn(drizzle, "select").mockImplementation(((fields: Parameters<typeof select>[0]) => {
          selects += 1;
          const query = select(fields);
          if (selects !== 2) return query;
          return {
            from: (table: typeof uploadChunk) => ({
              where: (where: Parameters<ReturnType<typeof query.from>["where"]>[0]) =>
                drizzle.delete(uploadChunk).where(eq(uploadChunk.deploymentId, raced)).pipe(Effect.andThen(query.from(table).where(where))),
            }),
          };
        }) as never);
        const extracted = yield* extractUpload(organizationId, raced);
        midRead.mockRestore();
        assert.strictEqual(selects, 2);
        assert.strictEqual(extracted, undefined);
        assert.strictEqual(yield* extractUpload(organizationId, raced), undefined);
      }).pipe(Effect.provide(cloud));
    }).pipe(Effect.scoped),
  60_000,
);

it.live(
  "a queued Deploy runs from the upload it claims once the Deployment ahead of it settled",
  () =>
    Effect.gen(function* () {
      const inngest = new Inngest({ id: "config-store-test" });
      vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const cloud = yield* cloudServices({}, inngest);
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const { id: organizationId } = (yield* resolveCaller(signedIn(alice))).organization;
        yield* enrollStoreServer(organizationId);
        yield* request("write", alice, shop);
        yield* request("write", alice, { command: "create_service", id: SERVICE, environment: here, name: "app", image: null });
        const store = yield* cloudStore;
        const { drizzle } = yield* Database;
        const servers = yield* drizzle.select().from(organizationMachine);
        // Admitted while its Server is there, run once it left, so nothing runs.
        yield* drizzle.delete(organizationMachine);
        const write = (command: ConfigCommand) => Effect.gen(function* () {
          yield* drizzle.insert(organizationMachine).values(servers);
          const written = yield* request("write", alice, command);
          yield* drizzle.delete(organizationMachine);
          assert.strictEqual(written.status, 200);
        });
        const run = (deploymentId: string) =>
          runStoreDeployment({ organizationId, environmentId: ENVIRONMENT, deploymentId, trigger: "user" }, "cloud-test");
        const deployment = (n: number) => `00000000-0000-4000-8000-000000000${n}`;
        const [first, second, retried, settingsOnly] = [deployment(301), deployment(302), deployment(303), deployment(304)];
        for (const [id, c] of [[first, "a"], [second, "b"]] as const) {
          assert.strictEqual((yield* upload(id, alice, yield* archiveOf(`FROM scratch\nLABEL upload=${c}\n`))).status, 200);
          yield* write({
            command: "admit", admit: "deploy", id, environment: here, services: [], version: null, accept_volume_loss: [],
            upload: { digest: c.repeat(64), base: null, uploader: null },
          });
          yield* run(id);
        }
        // A retry of the first runs, and a settings-only Deploy queues behind it, following it for now.
        yield* write({ command: "admit", admit: "retry", id: retried, deployment: first });
        yield* Effect.promise(() => store.claimDeployment(organizationId, retried, "cloud-retry"));
        yield* write({
          command: "admit", admit: "deploy", id: settingsOnly, environment: here, services: [], version: null, accept_volume_loss: [],
        });
        const queued = yield* Effect.promise(() => store.read(organizationId, { query: "deployment", id: settingsOnly }));
        expect(queued).toMatchObject({ upload: { digest: "a".repeat(64) }, chooses_upload_at_start: true });
        // The retry stops before it applies anything, so app stays on the second upload.
        yield* Effect.promise(() => store.abandonDeployment(retried, "cloud-retry"));
        let extracted: string | undefined;
        const runDeployment = store.runDeployment.bind(store);
        const handed = vi.spyOn(store, "runDeployment").mockImplementation(async (...args: Parameters<typeof runDeployment>) => {
          const upload = args[4]?.upload;
          extracted = upload === undefined ? undefined : await readFile(path.join(upload, "Dockerfile"), "utf8");
          return runDeployment(...args);
        });
        yield* run(settingsOnly);
        handed.mockRestore();
        assert.strictEqual(extracted, "FROM scratch\nLABEL upload=b\n", "Cloud extracts the upload the Deployment claimed");
        const ran = yield* Effect.promise(() => store.read(organizationId, { query: "deployment", id: settingsOnly }));
        assert.strictEqual(ran.upload?.digest, "b".repeat(64));
        assert.isUndefined(ran.chooses_upload_at_start);
      }).pipe(Effect.provide(cloud));
    }).pipe(Effect.scoped),
  60_000,
);

it.live(
  "a Read token can't upload source, and nothing is kept",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        yield* enrollStoreServer((yield* resolveCaller(signedIn(alice))).organization.id);
        yield* request("write", alice, shop);
        const { secret } = yield* createOrganizationToken(yield* resolveCaller(signedIn(alice)), {
          name: "reader", expiresInDays: 1, preset: { preset: "read" },
        });
        const deploymentId = "00000000-0000-4000-8000-000000000204";
        const refused = yield* upload(deploymentId, secret, yield* sourceArchive);
        assert.strictEqual(refused.status, 403);
        expect(refused.json.error).toMatchObject({ code: "forbidden", message: "This token's grant doesn't include changes:deploy." });
        const { drizzle } = yield* Database;
        assert.deepStrictEqual(yield* drizzle.select().from(uploadChunk).where(eq(uploadChunk.deploymentId, deploymentId)), []);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a user's write carries what they hold: none named is a defect, and callStoreBy sends the role's set",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const organizationId = "00000000-0000-4000-8000-00000000c001";
        const owner = yield* seedStoreOrganization(organizationId);
        const { drizzle } = yield* Database;
        const [linus] = yield* drizzle.insert(user).values({ email: "linus@example.test", name: "Linus" }).returning();
        const memberId = linus?.id ?? assert.fail("no member");
        yield* drizzle.insert(member).values({ userId: memberId, organizationId, role: "member" });
        const organization = { id: organizationId, slug: "shop" };
        const by = (userId: string, role: "owner" | "member"): Access => ({ kind: "member", userId, role, organization, permissions: permissionsOf(role), environments: undefined });
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
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

it.live(
  "a Deploy staging token deploys staging, and the Store refuses it production without writing",
  () =>
    Effect.gen(function* () {
      const cloud = yield* cloudServices();
      yield* Effect.gen(function* () {
        const alice = yield* signUp("alice");
        const staging = { project: "shop", environment: "staging" };
        for (const command of [shop, web, { command: "create_environment", id: "00000000-0000-4000-8000-000000000004", project: "shop", name: "staging" } as const]) {
          assert.strictEqual((yield* request("write", alice, command)).status, 200);
        }
        assert.strictEqual((yield* request("write", alice, { command: "create_service", id: "00000000-0000-4000-8000-000000000005", environment: staging, name: "web", image: "nginx:1" })).status, 200);
        const { secret, scope } = yield* createOrganizationToken(yield* resolveCaller(signedIn(alice)), {
          name: "ci", expiresInDays: 1, preset: { preset: "deploy", environment: "staging" },
        });
        assert.strictEqual(scope, "Deploy staging");
        const replicas = (environment: typeof here | typeof staging) => request("read", alice, { query: "environment", environment, path: "web.replicas", all: false });
        const before = (yield* replicas(here)).json;

        const refused = yield* request("write", secret, { command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.replicas", value: "3" }] });
        assert.strictEqual(refused.status, 403);
        expect(refused.json.error).toMatchObject({ code: "forbidden", message: "This token can deploy only to staging." });
        assert.deepStrictEqual((yield* replicas(here)).json, before);

        const landed = yield* request("write", secret, { command: "edit", environment: staging, expect: null, changes: [{ op: "set", path: "web.replicas", value: "3" }] });
        assert.strictEqual(landed.status, 200);
        assert.deepStrictEqual(landed.json.staged, ["web.replicas"]);
      }).pipe(Effect.provide(cloud));
    }),
  60_000,
);

const CEILING_ORGANIZATION = "00000000-0000-4000-8000-000000004401";
const protectIn = (environment: string, on: boolean): ConfigCommand => ({
  command: "set_environment_protected", environment: { project: "shop", environment }, protected: on,
});
/** Shop with a Protected production and a staging, and a token of `grant` minted into it. */
const ceilingShop = Effect.fn(function* (grant: TokenGrant, kind: "cli" | "agent") {
  const userId = yield* seedStoreOrganization(CEILING_ORGANIZATION);
  const store = yield* cloudStore;
  yield* Effect.promise(async () => {
    await store.write(CEILING_ORGANIZATION, {
      command: "create_project", id: "00000000-0000-4000-8000-000000004402", name: "shop", default_environment: "00000000-0000-4000-8000-000000004403",
    });
    await store.write(CEILING_ORGANIZATION, { command: "create_environment", id: "00000000-0000-4000-8000-000000004404", project: "shop", name: "staging" });
  });
  const { secret } = yield* mintOrganizationTokenUnchecked({
    organizationId: CEILING_ORGANIZATION, userId, name: "t", kind, expiresAt: new Date(Date.now() + 86_400_000), grant,
  });
  const headers = signedIn(secret);
  const state = Effect.promise(async () =>
    (await store.read(CEILING_ORGANIZATION, { query: "environments", project: "shop" })).environments.map(({ name, protected: on }) => [name, on]));
  const approvals = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select().from(operationApprovals).where(eq(operationApprovals.organizationId, CEILING_ORGANIZATION));
  });
  return { secret, headers, state, approvals };
});
const ceilingCloud = storeTestCloudWith(AuthLive);
const stagingOnly = "This token can deploy only to staging.";

it.live("a token that may protect only staging protects staging, asks a human to unprotect it, and production refuses", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { headers, state, approvals } = yield* ceilingShop({ permissions: ["environments:protect"], environments: ["staging"] }, "cli");
      for (const on of [false, true]) {
        const refused = yield* send("write", headers, protectIn("production", on));
        assert.deepStrictEqual([refused.status, refused.json.error?.code, refused.json.error?.message], [403, "forbidden", stagingOnly]);
      }
      assert.deepStrictEqual(yield* approvals, []);
      assert.strictEqual((yield* send("write", headers, protectIn("staging", true))).status, 200);
      assert.deepStrictEqual(yield* state, [["production", true], ["staging", true]]);
      const asked = yield* send("write", headers, protectIn("staging", false));
      assert.deepStrictEqual([asked.status, asked.json.error?.code], [409, "approval_required"]);
      assert.deepStrictEqual(yield* state, [["production", true], ["staging", true]]);
      assert.lengthOf(yield* approvals, 1);
    }).pipe(Effect.provide(yield* ceilingCloud));
  }), 60_000);

it.live("an agent token's unprotect outside its Environments is refused before it asks a human, on the config API and the sidebar", () =>
  Effect.gen(function* () {
    yield* Effect.gen(function* () {
      const { headers, state, approvals } = yield* ceilingShop({ permissions: ["environments:protect"], environments: ["staging"] }, "agent");
      const mcp = yield* send("write", headers, protectIn("production", false));
      assert.deepStrictEqual([mcp.status, mcp.json.error?.code, mcp.json.error?.message], [403, "forbidden", stagingOnly]);
      const caller = yield* resolveCaller(headers);
      const sidebar = yield* runAgentCall({ ...caller, via: "agent" }, {
        tool: "environment_protect", input: { protected: false }, key: "ceiling", approvalId: null,
        page: { at: "here", project: "shop", environment: "production" },
      });
      assert.deepStrictEqual(sidebar, { outcome: { ok: false, refusal: { code: "forbidden", message: stagingOnly, details: { environments: ["staging"] } } } });
      assert.deepStrictEqual(yield* approvals, []);
      assert.deepStrictEqual(yield* state, [["production", true], ["staging", false]]);
    }).pipe(Effect.provide(yield* ceilingCloud));
  }), 60_000);
