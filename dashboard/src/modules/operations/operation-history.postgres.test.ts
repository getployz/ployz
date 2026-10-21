import { it } from "@effect/vitest";
import type { MachineId } from "@ployz/sdk";
import { type SQL, sql } from "drizzle-orm";
import { Effect } from "effect";
import { describe, expect } from "vitest";
import { machineRemoveAttempt, serverDrainAttempt } from "#/modules/machines/tables";
import { operationsOn, settledDrain } from "#/modules/operations/operation-history.server";
import { serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d101";
const MACHINE = "b".repeat(32) as MachineId;
const FIRST = "00000000-0000-4000-8000-00000000d1c1";
const SECOND = "0000000000004000800000000000d1c2";
const THIRD = "00000000-0000-4000-8000-00000000d1c3";

const runs = (at: Record<"drain" | "remove" | "upgrade", Date | SQL>) =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
    const { drizzle } = yield* Database.pipe(Effect.provide(services));
    yield* drizzle.insert(serverDrainAttempt).values({ id: THIRD, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, requestedAt: at.drain });
    yield* drizzle.insert(machineRemoveAttempt).values({
      id: FIRST, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId, confirmDataLoss: [], createdAt: at.remove,
    });
    yield* drizzle.insert(serverUpgradeAttempt).values({
      organizationId: ORGANIZATION, machineId: MACHINE, attemptId: SECOND, trigger: "manual", requestedByUserId: userId,
      channel: "stable", fromVersion: "0.5.0", outcome: "succeeded", inngestRunId: "run-1", startedAt: at.upgrade, endedAt: new Date(),
    });
    return (limit: number) =>
      operationsOn(ORGANIZATION, { kind: "server", machine: MACHINE }, null, limit, "machine-1").pipe(
        Effect.map((operations) => operations.map(({ run }) => run)),
        Effect.provide(services),
      );
  });

describe("operationsOn a Server", () => {
  // Catches runs of different kinds requested within one second coming back in kind order, not newest first.
  it.live("lists runs requested within one second newest first", () =>
    Effect.gen(function* () {
      const read = yield* runs({
        drain: new Date("2026-10-20T12:00:00.100Z"),
        remove: new Date("2026-10-20T12:00:00.300Z"),
        upgrade: new Date("2026-10-20T12:00:00.500Z"),
      });

      expect(yield* read(5)).toEqual(["upgrade machine-1", "remove machine-1", "drain machine-1"]);
      expect(yield* read(1)).toEqual(["upgrade machine-1"]);
    }));

  it.live("lists runs a fraction of a millisecond apart newest first", () =>
    Effect.gen(function* () {
      const read = yield* runs({
        drain: sql`'2026-10-20 12:00:00.100100+00'::timestamptz`,
        remove: sql`'2026-10-20 12:00:00.100300+00'::timestamptz`,
        upgrade: sql`'2026-10-20 12:00:00.100200+00'::timestamptz`,
      });

      expect(yield* read(5)).toEqual(["remove machine-1", "upgrade machine-1", "drain machine-1"]);
    }));

  it.live("lists runs requested at the same instant by attempt", () =>
    Effect.gen(function* () {
      const at = new Date("2026-10-20T12:00:00.100Z");
      const read = yield* runs({ drain: at, remove: at, upgrade: at });

      expect(yield* read(5)).toEqual(["remove machine-1", "upgrade machine-1", "drain machine-1"]);
      expect(yield* read(5)).toEqual(yield* read(5));
      expect(yield* read(2)).toEqual(["remove machine-1", "upgrade machine-1"]);
    }));

  // Catches the upgrade query's limit keeping a different attempt than the merge ranks first, under a collation other than byte order.
  it.live("keeps the attempt the merge ranks first under an ICU numeric collation", () =>
    Effect.gen(function* () {
      const services = yield* storeTestCloud();
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.execute(sql`create collation numeric (provider = icu, locale = 'en-u-kn-true')`);
      yield* drizzle.execute(sql`alter table server_upgrade_attempt alter column attempt_id type text collate numeric`);
      const startedAt = new Date("2026-10-20T12:00:00.100Z");
      yield* drizzle.insert(serverUpgradeAttempt).values(["a2b00000000040008000000000000000", "a10b0000000040008000000000000000"].map((attemptId) => ({
        organizationId: ORGANIZATION, machineId: MACHINE, attemptId, trigger: "manual" as const, requestedByUserId: userId,
        channel: "stable" as const, fromVersion: "0.5.0", outcome: "succeeded" as const, inngestRunId: `run-${attemptId}`, startedAt, endedAt: new Date(),
      })));
      const read = (limit: number) =>
        operationsOn(ORGANIZATION, { kind: "server", machine: MACHINE }, null, limit, "machine-1").pipe(
          Effect.map((operations) => operations.map(({ attempt }) => attempt)),
          Effect.provide(services),
        );

      expect(yield* read(5)).toEqual(["a10b0000-0000-4000-8000-000000000000", "a2b00000-0000-4000-8000-000000000000"]);
      expect(yield* read(1)).toEqual(["a10b0000-0000-4000-8000-000000000000"]);
    }));
});

const second = (at: number) => new Date(Date.UTC(2026, 9, 20, 12, 0, at));
type Drain = Pick<typeof serverDrainAttempt.$inferInsert, "id" | "state" | "endCode" | "inngestRunId" | "startedAt" | "endedAt" | "requestedAt">;
const failed = (id: string, at: number): Drain => ({ id, state: "failed", endCode: "not_started", endedAt: second(at), requestedAt: second(at) });
const lost = (id: string, at: number): Drain => ({
  id, state: "unknown", endCode: "lost", inngestRunId: `run-${id}`, startedAt: second(at), endedAt: second(at), requestedAt: second(at),
});

/** A Server whose `drains` came first and `later` upgrades followed them, read through `settledDrain`. */
const drained = (drains: readonly Drain[], later: number) =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
    const { drizzle } = yield* Database.pipe(Effect.provide(services));
    for (const drain of drains) {
      yield* drizzle.insert(serverDrainAttempt).values({ ...drain, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
    }
    for (let at = 0; at < later; at++) {
      yield* drizzle.insert(serverUpgradeAttempt).values({
        organizationId: ORGANIZATION, machineId: MACHINE, attemptId: at.toString(16).padStart(32, "0"), trigger: "manual", requestedByUserId: userId,
        channel: "stable", fromVersion: "0.5.0", outcome: "succeeded", inngestRunId: `upgrade-${at}`, startedAt: second(30 + at), endedAt: second(30 + at),
      });
    }
    return yield* settledDrain(ORGANIZATION, MACHINE, "machine-1").pipe(Effect.provide(services));
  });

describe("settledDrain", () => {
  // Catches a failed Drain read off the page of a Server's latest runs, which twenty later runs fill.
  it.live("names the newest Drain with a verdict past twenty later runs", () =>
    Effect.gen(function* () {
      const pending: Drain = { id: FIRST, requestedAt: second(59) };

      expect(yield* drained([failed(THIRD, 1), pending], 25))
        .toEqual(expect.objectContaining({ attempt: THIRD, run: "drain machine-1", state: "failed", reason: "The drain never started." }));
      expect(yield* drained([failed(THIRD, 1), lost(FIRST, 2)], 25))
        .toEqual(expect.objectContaining({ attempt: FIRST, state: "unknown", end: "lost" }));
    }));

  it.live("names none when no Drain has a verdict", () =>
    Effect.gen(function* () {
      expect(yield* drained([{ id: THIRD, requestedAt: second(1) }], 3)).toBeNull();
    }));

  // Catches Drains requested at one instant picked in another order than `operationsOn` lists them.
  it.live("breaks a tie at one instant by attempt, as the history does", () =>
    Effect.gen(function* () {
      expect(yield* drained([failed(THIRD, 1), lost(FIRST, 1)], 0)).toEqual(expect.objectContaining({ attempt: FIRST }));
      expect(yield* drained([lost(FIRST, 1), failed(THIRD, 1)], 0)).toEqual(expect.objectContaining({ attempt: FIRST }));
    }));
});
