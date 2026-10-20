import { assert, it } from "@effect/vitest";
import type { ConfigCommand, MachineId } from "@ployz/sdk";
import { createHash } from "node:crypto";
import { eq } from "drizzle-orm";
import { Cause, Effect, Exit, Layer } from "effect";
import { runAgentCall } from "#/modules/agent/agent-runner.server";
import { callStoreAsMember, writeStoreAsMember } from "#/modules/config-store/config-store.server";
import { githubInstallation, githubRepositoryCache, organizationGithubInstallation } from "#/modules/github/tables";
import { resolveCaller } from "#/modules/identity/caller.server";
import { member, session as sessionTable } from "#/modules/identity/tables";
import { organizationMachine } from "#/modules/machines/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { Auth, AuthLive } from "#/server/auth.server";
import { encodePublicError, statusForPublicError } from "#/server/public-error";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { storeTestCloud } from "#/test/store-cloud";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const origin = "http://localhost:3000";
const here = { project: null, environment: null };

const cloudLayer = Effect.fn(function* () {
  const services = yield* storeTestCloud({ env: { PLOYZ_HOSTED_DNS_URL: "http://127.0.0.1:9/" } });
  return Layer.merge(AuthLive.pipe(Layer.provide(services)), services);
});

const signUp = Effect.fn(function* (name: string) {
  const response = yield* (yield* Auth).handler(new Request(`${origin}/api/auth/sign-up/email`, {
    method: "POST",
    headers: { "content-type": "application/json", "user-agent": "ployz-cli/0.0.0" },
    body: JSON.stringify({ email: `${name}@example.test`, name, password: "correct-horse-battery-staple" }),
  }));
  assert.strictEqual(response.status, 200);
  return response.headers.get("set-auth-token") ?? assert.fail("no session token");
});

const signedIn = (session: string) => new Headers({ authorization: `Bearer ${session}`, "content-type": "application/json" });

/** One write from the CLI as `session`: its status, and the Store's refusal if any. */
const cli = Effect.fn(function* (session: string, command: ConfigCommand) {
  const exit = yield* Effect.exit(handleConfigRequest(new Request(`${origin}/api/config/write`, {
    method: "POST", headers: signedIn(session), body: JSON.stringify(command),
  })));
  if (Exit.isFailure(exit)) return { status: statusForPublicError(encodePublicError(Cause.squash(exit.cause))), error: null };
  const json = (yield* Effect.promise(() => exit.value.json())) as { error?: { code: string; message: string } };
  return { status: exit.value.status, error: json.error === undefined ? null : [json.error.code, json.error.message] };
});

const set = (value: string): ConfigCommand => ({
  command: "edit", environment: here, expect: null, changes: [{ op: "set", path: "web.preferredBuilder", value }],
});

const one = machineId("1");
const two = machineId("2");
const twinA = machineId("3");
const twinB = machineId("4");
/** Named with machine-2's ID: an exact ID beats another Server's name. */
const namedLikeTwo = machineId("5");
const unlisted = machineId("7");
/** A Server of a pairing the Organization no longer has. */
const old = machineId("8");

/**
 * Alice owns Project shop, running `web` from acme/web, in an Organization paired with machine-1, machine-2, two
 * Servers both named twin, and one named like machine-2's ID; Bob is a member. Cloud holds each Server's name.
 */
const shop = Effect.fn(function* () {
  const session = yield* signUp("alice");
  const alice = yield* resolveCaller(signedIn(session));
  const organizationId = alice.organization.id;
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const secret = "ppair_fixture_names";
  yield* drizzle.insert(organizationPairing).values({
    organizationId, encryptedPairingSecret: encryption.encrypt(secret), founderPublicKey: "founder-key", founderClaimMachineId: one,
  });
  const servers: Array<[MachineId, string, string]> = [
    [one, "machine-1", secret], [two, "machine-2", secret], [twinA, "twin", secret], [twinB, "twin", secret],
    [namedLikeTwo, two, secret], [old, "machine-8", "ppair_fixture_before"],
  ];
  for (const [server, name, pairing] of servers) {
    yield* drizzle.insert(organizationMachine).values({
      organizationId, machineId: server, machineName: name, clusterKey: createHash("sha256").update(pairing).digest("hex"),
      encryptedCapability: encryption.encrypt(`ployz1:cloud:${server}`), isDialEntry: server === one,
    });
  }
  yield* drizzle.insert(githubInstallation).values({ installationId: 7, accountLogin: "acme", accountType: "Organization" });
  yield* drizzle.insert(organizationGithubInstallation).values({
    organizationId, installationId: 7, connectedByUserId: alice.userId, connectedGithubLogin: "alice",
  });
  yield* drizzle.insert(githubRepositoryCache).values({
    installationId: 7, repositoryId: 11, name: "web", fullName: "acme/web", defaultBranch: "main",
    private: true, htmlUrl: "https://github.com/acme/web", repoUpdatedAt: new Date(),
  });
  const slug = alice.organization.slug;
  assert.isTrue((yield* writeStoreAsMember(alice, slug, {
    command: "create_project", id: "00000000-0000-4000-8000-000000000001", name: "shop", default_environment: "00000000-0000-4000-8000-000000000002",
  })).ok);
  assert.isTrue((yield* writeStoreAsMember(alice, slug, {
    command: "create_git_service", id: "00000000-0000-4000-8000-000000000003", environment: here, name: "web", repository: "acme/web", branch: null,
  })).ok);
  const bobSession = yield* signUp("bob");
  const bob = yield* resolveCaller(signedIn(bobSession));
  yield* drizzle.insert(member).values({ userId: bob.userId, organizationId, role: "member" });
  yield* drizzle.update(sessionTable).set({ activeOrganizationId: organizationId, activeOrganizationSlug: slug })
    .where(eq(sessionTable.userId, bob.userId));
  /** web's Preferred Builder as the Store holds it. */
  const preferred = Effect.fn(function* () {
    const read = yield* callStoreAsMember(alice, slug, {
      operation: "read", query: { query: "environment", environment: here, path: "web.preferredBuilder", all: false },
    });
    return read.ok && read.value.view === "environment" ? read.value.settings[0]?.value : assert.fail("no Settings");
  });
  return { alice, session, bob: { userId: bob.userId, session: bobSession }, slug, preferred };
});

it.live(
  "the CLI's set resolves a Preferred Builder in Cloud: github, then an exact Machine ID, then a Server name, then an ID as typed",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { session, preferred } = yield* shop();
        const resolves: Array<[string, string]> = [
          ["github", "github"],
          ["machine-1", one],
          [" machine-2 ", two],
          [`${one} `, one],
          [two, two],
          [unlisted, unlisted],
        ];
        for (const [typed, stored] of resolves) {
          assert.deepStrictEqual(yield* cli(session, set(typed)), { status: 200, error: null }, typed);
          assert.strictEqual(yield* preferred(), stored, typed);
        }

        assert.deepStrictEqual(yield* cli(session, set("twin")), {
          status: 409, error: ["ambiguous", `Server name twin is ambiguous: ${twinA}, ${twinB}`],
        });
        assert.deepStrictEqual(yield* cli(session, set("machine-9")), {
          status: 404, error: ["not_found", "Server machine-9 was not found"],
        });
        assert.deepStrictEqual(yield* cli(session, set("machine-8")), {
          status: 404, error: ["not_found", "Server machine-8 was not found"],
        }, "a Server of an earlier pairing isn't listed");
        assert.strictEqual(yield* preferred(), unlisted, "a refused value writes nothing");
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "a Server name reaches the Store as its Machine ID from --patch, the agent's service_update and a member's dashboard",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, bob, slug, session, preferred } = yield* shop();

        assert.deepStrictEqual(yield* cli(session, {
          command: "edit", environment: here, expect: null, changes: [{ op: "patch", path: "web", value: { preferredBuilder: "machine-2" } }],
        }), { status: 200, error: null });
        assert.strictEqual(yield* preferred(), two, "--patch");

        const agent = { ...alice, via: "agent" as const };
        const page = { at: "here" as const, project: "shop", environment: "production" };
        const byAgent = yield* runAgentCall(agent, {
          tool: "service_update", input: { service: "web", set: { preferredBuilder: "machine-1" } }, key: "named", approvalId: null, page,
        });
        assert.deepInclude(byAgent, { outcome: { ok: true, value: { at: "shop/production", service: "web", changed: ["preferredBuilder"], immediate: ["preferredBuilder"], staged: false } } });
        assert.strictEqual(yield* preferred(), one, "the agent's service_update");
        const unknown = yield* runAgentCall(agent, {
          tool: "service_update", input: { service: "web", set: { preferredBuilder: "machine-9" } }, key: "unknown", approvalId: null, page,
        });
        assert.deepNestedInclude(unknown, { "outcome.ok": false, "outcome.refusal.code": "not_found" });

        assert.isTrue((yield* writeStoreAsMember(bob, slug, set("machine-2"))).ok, "a member names a Server");
        assert.strictEqual(yield* preferred(), two, "a member's dashboard write");
        assert.deepStrictEqual(yield* cli(bob.session, set("machine-1")), { status: 200, error: null }, "a member's CLI");
        assert.strictEqual(yield* preferred(), one);
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);

it.live(
  "only a Service's preferredBuilder is resolved: a variable of that name keeps the text",
  () =>
    Effect.gen(function* () {
      const layer = yield* cloudLayer();
      yield* Effect.gen(function* () {
        const { alice, slug } = yield* shop();
        assert.isTrue((yield* writeStoreAsMember(alice, slug, {
          command: "edit", environment: here, expect: null, changes: [
            { op: "set", path: "web.env.preferredBuilder", value: "machine-1" },
            { op: "patch", path: "web", value: { env: { PREFERRED: "machine-2" } } },
          ],
        })).ok);
        const read = yield* callStoreAsMember(alice, slug, {
          operation: "read", query: { query: "environment", environment: here, path: "web", all: false },
        });
        const values = read.ok && read.value.view === "environment" ? read.value.values : assert.fail("no Settings");
        assert.deepInclude(values?.["env"], { PREFERREDBUILDER: "machine-1", PREFERRED: "machine-2" });
      }).pipe(Effect.provide(layer));
    }),
  60_000,
);
