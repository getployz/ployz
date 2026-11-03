import { it } from "@effect/vitest";
import type { Connection } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import { callCloud } from "#/modules/agent/call-cloud.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { Caller } from "#/modules/identity/actor";
import { VolumeCopyObserver } from "#/modules/runtime/ployz.server";
import { observeOrganization } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { logged } from "#/test/logged";
import { enrollStoreServer, recordStoreSealingKey, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d701";
const PROJECT = "00000000-0000-4000-8000-00000000d702";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000d703";

it.live("the Volume-copy sweep and an agent's copies ask a Server of the pairing's Cluster once its Sealing Key is known, and log one they left", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const observeCopies = vi.fn(async (_connections: Connection[], _expect?: { readonly sealingPublicKey: string } | null) => ({ copies: [], unanswered: [], foreign: [] as string[] }));
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) =>
      program.pipe(Effect.provide(services), Effect.provideService(VolumeCopyObserver, { observe: observeCopies }));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const store = yield* provided(cloudStore);
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT }));
    yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.execute(sql`
        insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, 'shop-prod', ${ENVIRONMENT})`);
    }).pipe(Effect.orDie));
    const owner: Caller = {
      userId: "user-1", organization: { id: ORGANIZATION, slug: "shop" }, credential: { kind: "session", id: "session-1" }, role: "owner",
      support: null, via: "agent",
    };
    const servers = [{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }];
    const asked = Effect.gen(function* () {
      expect(yield* observeOrganization(ORGANIZATION)).toStrictEqual({ copies: [], unanswered: [] });
      const answer = yield* callCloud(owner, { ask: { query: "copies", namespace: "shop-prod", volume: null } });
      expect(answer).toMatchObject({ ok: true });
      expect(JSON.stringify(answer)).not.toMatch(new RegExp(`foreign|${machineId("9")}`));
    });

    // No Sealing Key recorded yet: any of the pairing's Servers, as before.
    yield* provided(asked);
    expect(observeCopies.mock.calls).toEqual([[servers, null], [servers, null]]);

    const pairingKey = `${"P".repeat(43)}=`;
    yield* provided(recordStoreSealingKey(ORGANIZATION, pairingKey));
    observeCopies.mockClear();
    observeCopies.mockResolvedValue({ copies: [], unanswered: [], foreign: [machineId("9")] });
    using errors = logged("ERROR", "in another Cluster", machineId("9"));
    yield* provided(asked);
    const expectation = { sealingPublicKey: pairingKey };
    expect(observeCopies.mock.calls).toEqual([[servers, expectation], [servers, expectation]]);
    expect(errors.count()).toBe(2);
  }), 60_000);
