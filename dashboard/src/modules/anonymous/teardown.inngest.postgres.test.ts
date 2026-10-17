import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { Client, ConfigCommand, MachineId } from "@ployz/sdk";
import { eq, isNotNull, sql } from "drizzle-orm";
import { Effect, Layer, Result } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { createScheduleAnonymousSweep, createTearDownAnonymousOrganization } from "#/modules/anonymous/teardown.inngest";
import { operationApprovals } from "#/modules/approvals/tables";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { claimSecretFor, hashClaimSecret } from "#/modules/identity/claim-required.server";
import { claimOrganization } from "#/modules/identity/claim.server";
import { member, organizationToken, user } from "#/modules/identity/tables";
import { createAnonymousTeardownRequestedEvent } from "#/modules/inngest/events";
import { organizationMachine } from "#/modules/machines/tables";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { organization, organizationChange } from "#/modules/organization/tables";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { uptimeCheck, uptimeIncident } from "#/modules/uptime/tables";
import { analyticsSalt, analyticsSite } from "#/modules/web-analytics/tables";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedAnonymousStoreOrganization, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-0000000a4001";
const OTHER = "00000000-0000-4000-8000-0000000a4002";
const THIRD = "00000000-0000-4000-8000-0000000a4003";
const FOURTH = "00000000-0000-4000-8000-0000000a4004";
const PROJECT = "00000000-0000-4000-8000-0000000a4010";
const ENVIRONMENT = "00000000-0000-4000-8000-0000000a4011";
const SERVICE = "00000000-0000-4000-8000-0000000a4012";
const QUEUED = "00000000-0000-4000-8000-0000000a4013";
const CHECK = "00000000-0000-4000-8000-0000000a4014";
const INCIDENT = "00000000-0000-4000-8000-0000000a4015";
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

/** Organization `ORGANIZATION` with a row in every table teardown clears, so `nothingLeft` proves each one went. */
const seeded = Effect.fn(function* (expiresAt: Date, has: { project?: boolean; server?: boolean } = {}) {
  const { userId } = yield* seedAnonymousStoreOrganization(ORGANIZATION, expiresAt);
  const { drizzle } = yield* Database;
  yield* drizzle.insert(analyticsSite).values({ organizationId: ORGANIZATION, environmentId: ENVIRONMENT, enabled: true });
  yield* drizzle.insert(analyticsSalt).values({ organizationId: ORGANIZATION, environmentId: ENVIRONMENT, day: "2026-10-17", salt: Buffer.from("salt") });
  yield* drizzle.insert(uptimeCheck).values({ id: CHECK, organizationId: ORGANIZATION, name: "web", url: "https://web.example.test/", environmentId: ENVIRONMENT, serviceId: SERVICE });
  yield* drizzle.insert(uptimeIncident).values({ id: INCIDENT, organizationId: ORGANIZATION, checkId: CHECK, checkRevision: 1, openedAt: new Date(), openedBy: { status: 503 } });
  yield* drizzle.insert(organizationChange).values({ organizationId: ORGANIZATION, sourceTable: "config_project", changedIds: [], deletedIds: [], allRows: true });
  if (has.server) {
    yield* enrollStoreServer(ORGANIZATION);
    yield* drizzle.update(organizationPairing).set({ founderMachineId: FOUNDER as MachineId });
    yield* drizzle.insert(observeIngestCursor).values({ organizationId: ORGANIZATION, machineId: machineId("0"), stream: "crash", seq: 1n });
    yield* drizzle.insert(crashEvent).values({
      organizationId: ORGANIZATION, machineId: machineId("0"), seq: 1n, serviceId: SERVICE, serviceName: "web", replica: "0", containerId: "c0", oom: false, occurredAt: new Date(),
    });
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

/** The claim URL's secret for `ORGANIZATION`, written where `claimOrganization` finds it. */
const claimable = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const secret = yield* claimSecretFor(ORGANIZATION);
  yield* drizzle.update(organization).set({ claimSecretHash: hashClaimSecret(secret) }).where(eq(organization.id, ORGANIZATION));
  return secret;
});

const ada = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [person] = yield* drizzle.insert(user).values({ email: "ada@example.test", name: "Ada" }).returning();
  return person?.id ?? "";
});

const owners = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ userId: member.userId }).from(member)
    .where(eq(member.organizationId, organizationId)).orderBy(member.userId);
  return rows.map((row) => row.userId);
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

/** Rows naming `organizationId` in every table of every schema with an `organization_id` column, the Store's included. */
const rowsNaming = Effect.fn(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const tables = yield* drizzle.execute<{ schema: string; name: string }>(sql`
    select c.table_schema as schema, c.table_name as name
    from information_schema.columns c
    join information_schema.tables t on t.table_schema = c.table_schema and t.table_name = c.table_name
    where c.table_schema not in ('pg_catalog', 'information_schema') and c.column_name = 'organization_id' and t.table_type = 'BASE TABLE'
    order by c.table_schema, c.table_name`, "objects");
  expect(tables.length).toBeGreaterThan(20);
  expect(tables.map((table) => table.schema)).toContain("analytics");
  expect(tables.some((table) => table.name.startsWith("config_"))).toBe(true);
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
  expect(yield* drizzle.select({ id: user.id }).from(user).where(eq(user.email, `${organizationId}@anonymous.ployz.invalid`))).toEqual([]);
});

const projects = (store: Store) =>
  Effect.promise(() => store.read(ORGANIZATION, { query: "projects" })).pipe(Effect.map((view) => view.projects.map((project) => project.name)));

const organizationIds = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ id: organization.id }).from(organization);
});

const pairingRows = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ id: organizationPairing.organizationId, removalStartedAt: organizationPairing.removalStartedAt }).from(organizationPairing);
});

const backdateRemoval = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.update(organizationPairing).set({ removalStartedAt: new Date(Date.now() - 2 * HOUR) }).where(isNotNull(organizationPairing.removalStartedAt));
  yield* drizzle.execute(sql`update config_removal_mark set at = at - 7200 where organization_id = ${ORGANIZATION}`);
});

it.live("leaves an Organization alone while its agent's token is live", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() + HOUR), { project: true });
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "live", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("leaves a claimed Organization alone however long ago the agent's token lapsed", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() + HOUR), { project: true });
    const person = yield* ada;
    yield* claimOrganization({ userId: person }, yield* claimable);
    const { drizzle } = yield* Database;
    yield* drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() - 48 * HOUR) });
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "claimed", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* owners(ORGANIZATION)).toEqual([person]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("leaves an Organization a person owns alongside the agent alone, and the sweep skips it", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000), { project: true });
    const person = yield* ada;
    const { drizzle } = yield* Database;
    yield* drizzle.insert(member).values({ userId: person, organizationId: ORGANIZATION, role: "owner" });
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "claimed", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);
    expect((yield* sweep(services)).result).toEqual({ organizationIds: [] });
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

it.live("removes only this Organization's agent: another sandbox's orphaned agent stays", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    const other = yield* seedAnonymousStoreOrganization(OTHER, new Date(Date.now() + HOUR));
    const { drizzle } = yield* Database;
    yield* drizzle.delete(member).where(eq(member.userId, other.userId));
    const run = yield* tearDown(services);
    expect(run.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
    expect(yield* drizzle.select({ id: user.id }).from(user).where(eq(user.id, other.userId))).toEqual([{ id: other.userId }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("removes an Organization with a Project in two runs: the Project, then the Organization", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000), { project: true });
    const first = yield* tearDown(services);
    expect(first.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_projects" });
    expect(yield* projects(store)).toEqual([]);
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);
    const second = yield* tearDown(services);
    expect(second.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
    const third = yield* tearDown(services);
    expect(third.result).toEqual({ organizationId: ORGANIZATION, state: "gone", did: "nothing" });
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("a claim that lands while teardown reads the Store keeps the Organization and its Project", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000), { project: true });
    const secret = yield* claimable;
    const person = yield* ada;
    const run = Effect.runPromiseWith(services);
    const { drizzle } = yield* Database;
    const original = store.read.bind(store);
    const read = vi.spyOn(store, "read").mockImplementationOnce(async (...args) => {
      await run(drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() + HOUR) }));
      await run(claimOrganization({ userId: person }, secret));
      return original(...args);
    });
    const step = yield* tearDown(services).pipe(Effect.ensuring(Effect.sync(() => read.mockRestore())));
    expect(step.result).toEqual({ organizationId: ORGANIZATION, state: "claimed", did: "nothing" });
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* owners(ORGANIZATION)).toEqual([person]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("a claim that lands while teardown removes the Store's rows keeps the Organization", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 60_000));
    const secret = yield* claimable;
    const person = yield* ada;
    const run = Effect.runPromiseWith(services);
    const { drizzle } = yield* Database;
    const original = store.removeOrganization.bind(store);
    const remove = vi.spyOn(store, "removeOrganization").mockImplementationOnce(async (...args) => {
      await run(drizzle.update(organizationToken).set({ expiresAt: new Date(Date.now() + HOUR) }));
      await run(claimOrganization({ userId: person }, secret));
      return original(...args);
    });
    const step = yield* tearDown(services).pipe(Effect.ensuring(Effect.sync(() => remove.mockRestore())));
    expect(step.result).toEqual({ organizationId: ORGANIZATION, state: "claimed", did: "nothing" });
    expect(yield* owners(ORGANIZATION)).toEqual([person]);
    expect(yield* drizzle.select({ id: analyticsSite.organizationId }).from(analyticsSite)).toEqual([{ id: ORGANIZATION }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("a claim decides liveness by the database clock, not the process's", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 1_000));
    const secret = yield* claimable;
    const person = yield* ada;
    const clock = vi.spyOn(Date, "now").mockReturnValue(Date.now() - 2 * HOUR);
    const claimed = yield* Effect.result(claimOrganization({ userId: person }, secret)).pipe(Effect.ensuring(Effect.sync(() => clock.mockRestore())));
    expect(Result.isFailure(claimed)).toBe(true);
    if (Result.isFailure(claimed)) expect(claimed.failure).toMatchObject({ _tag: "Conflict", message: "This sandbox expired." });
    expect(yield* owners(ORGANIZATION)).not.toContain(person);
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

it.live("forgets a Server that stayed unreachable an hour after its Project's removal began, and finishes", () => Effect.gen(function* () {
  const services = yield* cloud("unreachable");
  yield* Effect.gen(function* () {
    const { store } = yield* seeded(new Date(Date.now() - 2 * HOUR), { project: true, server: true });
    const { drizzle } = yield* Database;
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect(yield* runnerFails(store)).toBe(1);
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect(yield* runnerFails(store)).toBe(1);
    expect(yield* projects(store)).toEqual(["shop"]);
    expect(yield* pairingRows).toEqual([{ id: ORGANIZATION, removalStartedAt: null }]);
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);

    yield* backdateRemoval;
    const forgotten = yield* tearDown(services);
    expect(forgotten.result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_projects" });
    expect(forgotten.ctx.step.sendEvent).toHaveBeenCalledWith("request-next-step", [createAnonymousTeardownRequestedEvent({ organizationId: ORGANIZATION })]);
    expect(yield* projects(store)).toEqual([]);
    expect(yield* pairingRows).toEqual([]);
    expect(yield* drizzle.select().from(organizationMachine)).toEqual([]);
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("forgets a Server that stayed unreachable an hour after its pairing's revoke began, never on one failed probe", () => Effect.gen(function* () {
  const services = yield* cloud("unreachable");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 2 * HOUR), { server: true });
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    const [pairing] = yield* pairingRows;
    expect(pairing?.id).toBe(ORGANIZATION);
    expect(pairing?.removalStartedAt).not.toBeNull();
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);

    yield* backdateRemoval;
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("never forgets a Server that answers, however long the token and the removal have lapsed", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 48 * HOUR), { server: true });
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    yield* backdateRemoval;
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "waited_on_servers" });
    const [pairing] = yield* pairingRows;
    expect(pairing?.id).toBe(ORGANIZATION);
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("leftover rows go with the Organization or not at all: a failed delete keeps the Organization for a retry", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`create function fail_cleanup() returns trigger language plpgsql as $$ begin raise exception 'cleanup unavailable'; end $$`);
    yield* drizzle.execute(sql`create trigger fail_cleanup before delete on analytics.site for each row execute function fail_cleanup()`);
    const failed = yield* tearDown(services);
    expect(failed.error).toBeDefined();
    expect(yield* organizationIds).toEqual([{ id: ORGANIZATION }]);
    expect(yield* drizzle.select({ id: analyticsSite.organizationId }).from(analyticsSite)).toEqual([{ id: ORGANIZATION }]);
    expect((yield* sweep(services)).result).toEqual({ organizationIds: [ORGANIZATION] });

    yield* drizzle.execute(sql`drop trigger fail_cleanup on analytics.site`);
    expect((yield* tearDown(services)).result).toEqual({ organizationId: ORGANIZATION, state: "lapsed", did: "removed_organization" });
    yield* nothingLeft(ORGANIZATION);
  }).pipe(Effect.provide(services));
}), 60_000);

it.live("the sweep names only Organizations an anonymous agent alone owns whose token lapsed", () => Effect.gen(function* () {
  const services = yield* cloud("answering");
  yield* Effect.gen(function* () {
    yield* seeded(new Date(Date.now() - 60_000));
    yield* seedAnonymousStoreOrganization(OTHER, new Date(Date.now() + HOUR));
    yield* seedStoreOrganization(THIRD);
    yield* seedAnonymousStoreOrganization(FOURTH, new Date(Date.now() - HOUR));
    const { drizzle } = yield* Database;
    const [person] = yield* drizzle.select({ id: user.id }).from(user).where(eq(user.email, "ada@example.test"));
    yield* drizzle.insert(member).values({ userId: person?.id ?? "", organizationId: FOURTH, role: "owner" });
    const run = yield* sweep(services);
    expect(run.result).toEqual({ organizationIds: [ORGANIZATION] });
  }).pipe(Effect.provide(services));
}), 60_000);
