import { createHash } from "node:crypto";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { Client, ConfigCommand, MachineId } from "@ployz/sdk";
import { Effect, Layer, Schema } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { createDeployToFirstServer } from "#/modules/config-store/store-deployment.inngest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { member, user } from "#/modules/identity/tables";
import { createConfigFirstServerJoinedEvent } from "#/modules/inngest/events";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { CLUSTER_UNREACHABLE, enrollMachine, hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { checkForgetServers, forgetServers } from "#/modules/machines/forget-servers.server";
import {
  enrollmentAllocation, machineEnrollmentToken, machineRemoveAttempt, organizationMachine,
} from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f001";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000f003";
const QUEUED = "00000000-0000-4000-8000-00000000f101";
const FOUNDER = "0".repeat(32);
const here = { project: null, environment: null };
const TOKEN = "pmet_fixture_forget";

/** Cloud whose every connection attempt to a Server connects (`answers`) or fails. */
const cloud = (answers: boolean) => Effect.gen(function* () {
  const ployz = makePloyzLayer({
    connect: async () => {
      if (!answers) throw new Error("no route to the Server");
      return asTestDouble<Client>()({ close: async () => undefined });
    },
  });
  return yield* Layer.build(Layer.merge(yield* storeTestCloud(), ployz));
});

/** A signed-up owner's Organization whose founder completed, a Project with a queued Deploy, and enrollment rows. */
const seeded = Effect.fn(function* () {
  const userId = yield* seedStoreOrganization(ORGANIZATION);
  yield* enrollStoreServer(ORGANIZATION);
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationPairing).set({ founderMachineId: FOUNDER as MachineId });
  yield* drizzle.insert(machineEnrollmentToken).values({
    organizationId: ORGANIZATION, createdByUserId: userId, tokenHash: hashEnrollmentToken(TOKEN), expiresAt: new Date(Date.now() + 3_600_000),
  });
  yield* drizzle.insert(machineRemoveAttempt).values({ organizationId: ORGANIZATION, requestedByUserId: userId, machineId: FOUNDER as MachineId, confirmDataLoss: [] });
  const store = yield* cloudStore;
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  yield* write({ command: "create_project", id: "00000000-0000-4000-8000-00000000f002", name: "shop", default_environment: ENVIRONMENT });
  yield* write({ command: "create_service", id: "00000000-0000-4000-8000-00000000f004", environment: here, name: "web", image: "nginx:1" });
  yield* write({ command: "admit", admit: "deploy", id: QUEUED, environment: here, services: [], version: null, accept_volume_loss: [] });
  return { userId, store };
});

it.live("Forget Servers refuses while a Server answers, and forgets nothing", () => Effect.gen(function* () {
  const services = yield* cloud(true);
  yield* Effect.gen(function* () {
    const { userId } = yield* seeded();
    const refused = yield* forgetServers({ userId, organizationId: ORGANIZATION });
    expect(refused).toEqual({
      ok: false,
      refusal: {
        code: "conflict",
        message: `Your Servers are still reachable (${FOUNDER}). Remove them from the Servers page or with \`ployz server rm\` instead.`,
        details: { answered: [FOUNDER] },
      },
    });
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(organizationPairing)).toHaveLength(1);
    expect(yield* drizzle.select().from(organizationMachine)).toHaveLength(1);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("forgetting unreachable Servers clears Cloud's hold, and the next Server founds a new Cluster that redeploys", () => Effect.gen(function* () {
  const services = yield* cloud(false);
  yield* Effect.gen(function* () {
    const { userId, store } = yield* seeded();
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    const [before] = yield* drizzle.select().from(organizationPairing);
    // A member who isn't an owner or admin can't.
    const [bob] = yield* drizzle.insert(user).values({ email: "bob@example.test", name: "Bob" }).returning();
    yield* drizzle.insert(member).values({ userId: bob?.id ?? "", organizationId: ORGANIZATION, role: "member" });
    expect(yield* checkForgetServers({ userId: bob?.id ?? "", organizationId: ORGANIZATION }))
      .toMatchObject({ ok: false, refusal: { code: "forbidden" } });
    // A founding claim a Server may still be completing isn't taken away.
    yield* drizzle.update(organizationPairing).set({ founderMachineId: null });
    expect(yield* forgetServers({ userId, organizationId: ORGANIZATION })).toMatchObject({
      ok: false, refusal: { message: "A Server is still joining. Try again in a few minutes." },
    });
    yield* drizzle.update(organizationPairing).set({ founderMachineId: FOUNDER as MachineId });

    const forgotten = yield* forgetServers({ userId, organizationId: ORGANIZATION });
    expect(forgotten).toEqual({
      ok: true,
      value: { servers: [{ id: FOUNDER, name: FOUNDER, reach: "didnt_answer" }], volumes: [], cancelled: [QUEUED] },
    });
    for (const table of [organizationPairing, organizationMachine, machineEnrollmentToken, enrollmentAllocation, machineRemoveAttempt]) {
      expect(yield* drizzle.select().from(table)).toEqual([]);
    }
    // History keeps the Deploy that might have run, cancelled.
    expect(yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: QUEUED }))).toMatchObject({ status: "cancelled" });

    // A new token's first Server founds a new Cluster, under a new pairing secret.
    yield* drizzle.insert(machineEnrollmentToken).values({
      organizationId: ORGANIZATION, createdByUserId: userId, tokenHash: hashEnrollmentToken(`${TOKEN}_2`), expiresAt: new Date(Date.now() + 3_600_000),
    });
    const enrolled = yield* enrollMachine({ token: `${TOKEN}_2`, identity: identity("1".repeat(32)) });
    expect(enrolled).toMatchObject({ kind: "initialize", resumed: false });
    const secret = "pairing" in enrolled ? enrolled.pairing.secret : "";
    expect(secret).not.toBe(before && encryption.decrypt(before.encryptedPairingSecret));
    // It publishes its connection under the new pairing, as enrollment's callback does.
    yield* drizzle.insert(organizationMachine).values({
      organizationId: ORGANIZATION, machineId: "1".repeat(32) as MachineId, clusterKey: hashEnrollmentToken(secret),
      encryptedCapability: encryption.encrypt("ployz1:new"), isDialEntry: true,
    });

    // Its join redeploys the published Environment: nothing asks to delete data the old Servers took.
    const runner: Parameters<typeof createDeployToFirstServer>[1] =
      makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
    const joined = yield* Effect.promise(() => new InngestTestEngine({
      function: createDeployToFirstServer(new Inngest({ id: "forget-servers-test" }), runner),
      events: [createConfigFirstServerJoinedEvent({ organizationId: ORGANIZATION, machineId: "1".repeat(32) })],
    }).execute());
    expect(joined.result).toEqual([{ environment: "shop/production", admitted: true }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("forgetting clears a pairing whose removal is stuck waiting on Servers that are gone", () => Effect.gen(function* () {
  const services = yield* cloud(false);
  yield* Effect.gen(function* () {
    const userId = yield* seedStoreOrganization(ORGANIZATION);
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    const joiner = "2".repeat(32);
    yield* drizzle.insert(organizationPairing).values({
      organizationId: ORGANIZATION, encryptedPairingSecret: encryption.encrypt("ppair_stuck"), founderPublicKey: "founder-key",
      founderClaimMachineId: FOUNDER as MachineId, founderMachineId: FOUNDER as MachineId, removalStartedAt: new Date(),
      removalEndpoints: [
        { status: "pending", machineId: FOUNDER as MachineId, encryptedExpected: encryption.encrypt("ployz1:cloud") },
        { status: "unknown", machineId: joiner as MachineId },
      ],
    });
    const forgotten = yield* forgetServers({ userId, organizationId: ORGANIZATION });
    expect(forgotten).toMatchObject({
      ok: true,
      value: { servers: [{ id: FOUNDER, reach: "didnt_answer" }, { id: joiner, reach: "no_connection" }] },
    });
    expect(yield* drizzle.select().from(organizationPairing)).toEqual([]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("a join to a Cluster that doesn't answer fails at once, naming Forget Servers", () => Effect.gen(function* () {
  const services = yield* cloud(false);
  yield* Effect.gen(function* () {
    yield* seeded();
    const failure = yield* enrollMachine({ token: TOKEN, identity: identity("3".repeat(32)) }).pipe(
      Effect.provideService(OrganizationRuntime, {
        cancel: () => Effect.void,
        open: () => Effect.succeed({ status: "unreachable" as const, error: null }),
      }),
      Effect.flip,
    );
    expect(failure).toMatchObject({ _tag: "Conflict", message: CLUSTER_UNREACHABLE, userFacing: true });
    // So does one while another Server's founding claim went stale, rather than waiting on it forever.
    const { drizzle } = yield* Database;
    yield* drizzle.update(organizationPairing).set({ founderMachineId: null, createdAt: new Date(Date.now() - 11 * 60_000) });
    const stale = yield* Effect.flip(enrollMachine({ token: TOKEN, identity: identity("3".repeat(32)) }));
    expect(stale).toMatchObject({ _tag: "Conflict", message: CLUSTER_UNREACHABLE });
  }).pipe(Effect.provide(services));
}), 60_000);

function identity(machineId: string) {
  return {
    protocolVersion: 1 as const,
    machineId: Schema.decodeUnknownSync(rustMachineIdSchema)(machineId),
    initialPolicy: { labels: {}, accepts_builds: true, accepts_services: true, accepts_ingress: true },
    name: `server-${machineId.slice(0, 4)}`,
    publicKey: createHash("sha256").update(machineId).digest("base64"),
    advertisedEndpoints: ["10.0.0.9:51820"],
    requestedStorage: "none" as const,
  };
}
