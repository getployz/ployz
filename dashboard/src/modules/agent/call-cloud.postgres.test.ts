import { it } from "@effect/vitest";
import type { MachineId } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { describe, expect } from "vitest";
import { callCloud } from "#/modules/agent/call-cloud.server";
import type { Caller } from "#/modules/identity/actor";
import { serverDrainAttempt } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c101";
const DRAIN = "00000000-0000-4000-8000-00000000c102";
const MACHINE = "a".repeat(32) as MachineId;

describe("callCloud", () => {
  it.live("answers an operations query as OperationsAnswer, the shape the agent tools decode", () =>
    Effect.gen(function* () {
      const services = yield* Layer.build(yield* storeTestCloud());
      const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
      const { drizzle } = yield* Database.pipe(Effect.provide(services));
      yield* drizzle.insert(serverDrainAttempt).values({ id: DRAIN, organizationId: ORGANIZATION, machineId: MACHINE, requestedByUserId: userId });
      const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" } };

      const heard = yield* callCloud(caller, { ask: { query: "operations", subject: { kind: "server", machine: MACHINE }, attempt: null, limit: 5 } })
        .pipe(Effect.provide(services));

      expect(heard).toEqual({
        ok: true,
        value: { operations: [expect.objectContaining({ attempt: DRAIN, run: `drain ${MACHINE}`, state: "accepted" })] },
      });
    }));
});
