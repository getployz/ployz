import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { Client, ConfigCommand, MachineId } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { createScheduleAnonymousSweep, createTearDownAnonymousOrganization } from "#/modules/anonymous/teardown.inngest";
import { operationApprovals } from "#/modules/approvals/tables";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { createAnonymousTeardownRequestedEvent } from "#/modules/inngest/events";
import { organizationMachine } from "#/modules/machines/tables";
import { organization } from "#/modules/organization/tables";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { enrollStoreServer, seedAnonymousStoreOrganization, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-0000000a4001";
const OTHER = "00000000-0000-4000-8000-0000000a4002";
const PROJECT = "00000000-0000-4000-8000-0000000a4010";
const ENVIRONMENT = "00000000-0000-4000-8000-0000000a4011";
const SERVICE = "00000000-0000-4000-8000-0000000a4012";
const QUEUED = "00000000-0000-4000-8000-0000000a4013";
const FOUNDER = "0".repeat(32);
const HOUR = 3_600_000;
const here = { project: null, environment: null };

type Services = Effect.Success<ReturnType<typeof cloud>>;
type Store = Effect.Success<typeof cloudStore>;

const cloud = (servers: "answering" | "unreachable") => Effect.gen(function* () {
  const ployz = makePloyzLayer({
    connect: async () => {
      if (servers === "unreachable") throw new Error("no route to the Server");
      return asTestDouble<Client>()({ close: async () => undefined });
    },
  });
  return yield* Layer.build(Layer.merge(yield* storeTestCloud(), ployz));
});

const seeded = Effect.fn(function* (expiresAt: Date, has: { project?: boolean; server?: boolean } = {}) {
  const { userId } = yield* seedAnonymousStoreOrganization(ORGANIZATION, expiresAt);
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsSite).values({ organizationId: ORGANIZATION, environmentId: ENVIRONMENT, enabled: true });
  yield* drizzle.insert(analyticsSalt).values({ organizationId: ORGANIZATION, environmentId: ENVIRONMENT, day: "2026-10-17", salt: Buffer.from("salt") });
  if (has.server) {
    yield* enrollStoreServer(ORGANIZATION);
    yield* drizzle.update(organizationPairing).set({ founderMachineId: FOUNDER as MachineId });
  }
  const store = yield* cloudStore;
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  if (has.project) {
    yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
    yield* write({ command: "create_service", id: SERVICE, environment: here, name: "web", image: "nginx:1" });
    if (has.server) {
      yield* write({ command: "admit", admit: "deploy", id: QUEUED, environment: here, services: [], version: null, accept_volume_loss: [] });
      yield* Effect.promise(() => store.runDeployment(ORGANIZATION, QUEUED, "cloud-test", [], { failure: "the Server stopped answering" }));
    }
  }
  return { userId, store };
});

const tearDown = (services: Services, organizationId = ORGANIZATION) => {
  const runner: Parameters<typeof createTearDownAnonymousOrganization>[1] =
    makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
  const worker = createTearDownAnonymousOrganization(new Inngest({ id: "anonymous-teardown-test" }), runner);
  return Effect.promise(() => new InngestTestEngine({
    function: worker,
    events: [createAnonymousTeardownRequestedEvent({ organizationId })],
    steps: [{ id: "request-next-step", handler: () => ({ ids: ["event-1"] }) }],
  }).execute());
};

const runnerFails = (store: Store) => Effect.gen(function* () {
  const queued = yield* Effect.promise(() => store.unclaimed(Math.floor(Date.now() / 1000) + 1));
  for (const { deployment } of queued.filter((row) => row.organization === ORGANIZATION)) {
    yield* Effect.promise(() => store.runDeployment(ORGANIZATION, deployment, "cloud-test", [], { failure: "the Server stopped answering" }));
  }
  return queued.length;
});

const sweep = (services: Services) => {
  const runner: Parameters<typeof createScheduleAnonymousSweep>[1] =
    makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
  const worker = createScheduleAnonymousSweep(new Inngest({ id: "anonymous-sweep-test" }), runner);
  return Effect.promise(() => new InngestTestEngine({
    function: worker, steps: [{ id: "request-teardowns", handler: () => ({ ids: ["event-1"] }) }],
  }).execute());
};

const rowsNaming = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const tables = yield* drizzle.execute<{ schema: string; name: string }>(sql`
    select c.table_schema as schema, c.table_name as name
    from information_schema.columns c
    join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name
    where c.table_schema in ('public', 'analytics') and c.column_name = 'organization_id' and t.table_type = 'BASE TABLE'
    order by c.table_schema, c.table_name`, "objects");
  expect(tables.length).toBeGreaterThan(20);
  const counts: Record<string, number> = {};
  for (const { schema, name } of tables) {
    const [row] = yield* drizzle.execute<{ count: number }>(sql`
      select count(*)::int as count from ${sql.identifier(schema)}.${sql.identifier(name)} where organization_id = ${organizationId}`, "objects");
    counts[`${schema}.${name}`] = row?.count ?? -1;
  }
  return counts;
});

const nothingLeft = Effect.fn(function* (organizationId: string) {
  const counts = yield* rowsNaming(organizationId);
  expect(Object.entries(counts).filter(([, count]) => count !== 0)).toEqual([]);
  const { drizzle } = yield* Database;
  expect(yield* drizzle.select({ id: user.id }).from(user).where(eq(user.isAnonymous, true))).toEqual([]);
});

const projects = (store: Store) =>
  Effect.promise(() => store.read(ORGANIZATION, { query: "projects" })).pipe(Effect.map((view) => view.projects.map((project) => project.name)));

it.live("leaves an Organization alone while its agent's token is live", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() + HOUR), { project: true });
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "live", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select({ id: organization.id }).from(organization)).toEqual([{ id: ORGANIZATION }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("leaves a claimed Organization alone however long ago the agent's token lapsed", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store, userId } = yield* seeded(new Date(Date.now() - 48 * HOUR), { project: true });
    const { drizzle } = yield* Database;
    const [ada] = yield* drizzle.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning();
    yield* drizzle.insert(member).values({ userId: ada?.id ?? "", organizationId: ORGANIZATION, role: "owner" });
    yield* drizzle.delete(member).where(eq(member.userId, userId));
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "claimed", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    const [kept] = yield* drizzle.select({ claimSecretHash: organization.claimSecretHash }).from(organization);
    expect(kept?.claimSecretHash).not.toBeNull();
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("removes an empty Organization and every row that named it, and its agent", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("removes an Organization with a Project in two runs: the Project, then the Organization", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000), { project: true });
    const first = yield* tearDown(services);
    expect(first.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_projects" });
    expect(yield* projects(store)).toEqual([]);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select({ id: organization.id }).from(organization)).toEqual([{ id: ORGANIZATION }]);
    const second = yield* tearDown(services);
    expect(second.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
    const third = yield* tearDown(services);
    expect(third.result).toEqual({ organizationId: ORGANIZATION, state: "gone", did: "cleaned_up" });
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("removes a Project whose Services are on a Server without asking anyone's approval", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000), { project: true, server: true });
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect(run.ctx.step.sendEvent).not.toHaveBeenCalled();
    expect(yield* runnerFails(store)).toBe(1);
    expect(yield* projects(store)).toEqual(["shop"]);
    const { drizzle } = yield* Database;
    expect(yield* drizzle.select().from(operationApprovals)).toEqual([]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("forgets a Server that stopped answering, an hour past the token, and finishes", () => Effect.gen(function* () {
  const services = yield* cloud("unreachable");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 30 * 60_000), { project: true, server: true });
    const { drizzle } = yield* Database;
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect(yield* runnerFails(store)).toBe(1);
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect(yield* runnerFails(store)).toBe(1);
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* drizzle.select({ id: organizationPairing.organizationId }).from(organizationPairing)).toEqual([{ id: ORGANIZATION }]);
    expect(yield* drizzle.select({ id: organization.id }).from(organization)).toEqual([{ id: ORGANIZATION }]);

    yield* drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() - 2 * HOUR) });
    const forgotten = yield* tearDown(services);
    expect(forgotten.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_projects" });
    expect(forgotten.ctx.step.sendEvent).toHaveBeenCalledWith("request-next-step", [createAnonymousTeardownRequestedEvent({ organizationId: ORGANIZATION })]);
    expect(yield* projects(store)).toEqual([]);
    expect(yield* drizzle.select().from(organizationPairing)).toEqual([]);
    expect(yield* drizzle.select().from(organizationMachine)).toEqual([]);
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("a retry after a crash between the Organization's removal and its cleanup finishes the cleanup", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    const { drizzle } = yield* Database;
    yield* drizzle.delete(organization).where(eq(organization.id, ORGANIZATION));
    expect(Object.values(yield* rowsNaming(ORGANIZATION)).some((count) => count > 0)).toBe(true);
    expect(yield* drizzle.select({ id: user.id }).from(user).where(eq(user.isAnonymous, true))).toHaveLength(1);
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "gone", did: "cleaned_up" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("the sweep names only Organizations an anonymous agent owns whose token lapsed", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    yield* seedAnonymousStoreOrganization(OTHER, new Date(Date.now() + HOUR));
    yield* seedStoreOrganization("00000000-0000-4000-8000-0000000a4003");
    const run = yield* sweep(services);
    expect(run.result).toEqual({ organizationIds: [ORGANIZATION] });
  }).pipe(Effect.provide(services));
}), 60_000);
