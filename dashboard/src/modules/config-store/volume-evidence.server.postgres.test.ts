import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigQuery, ConfigWritten, Connection, SystemEvent, VolumeObservation } from "@ployz/sdk";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import type { EncryptedSecretValue, JsonObject, JsonValue } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { callStore, storeSystem } from "#/modules/config-store/config-store.server";
import { permissionsOf } from "#/modules/organization/permissions";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import type { StoreRead } from "#/modules/config-store/store.contract";
import { gatherUnanswered, gatherVolumeEvidence, systemVolumeEvidence, VolumeObserver } from "#/modules/config-store/volume-evidence.server";
import { organizationMachine } from "#/modules/machines/tables";
import { RpcError } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e701";
const ids = { project: "00000000-0000-4000-8000-00000000e702", environment: "00000000-0000-4000-8000-00000000e703", service: "00000000-0000-4000-8000-00000000e704" };
const staging = { project: "shop", environment: "staging" };
const here = { project: null, environment: null };

/** A command as it arrives raw from a caller, before the Store's own decoding judges it. */
const raw = asTestDouble<ConfigCommand>();

/** Which removals query, if any, gathering evidence for `command` asks the Store. */
const asked = (command: JsonObject) => Effect.gen(function* () {
  const read = vi.fn(async (query: ConfigQuery) => query.query === "environments"
    ? { view: "environments", environments: [{ name: "production" }, { name: "staging" }] }
    : { view: "removals", volumes: [] });
  const evidence = yield* gatherVolumeEvidence(ORGANIZATION, { operation: "write", command: raw(command) }, asTestDouble<StoreRead>()(read));
  expect(evidence).toBeUndefined();
  return read.mock.calls.map(([query]) => query);
});

it.live("a Publish or a Deploy of named Services gathers what the Servers hold of the Volumes it removes, as a full Deploy does", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const removals = (environment: JsonValue, remove: boolean) => [{ query: "removals", environment, remove }];
    const gathered = (command: JsonObject) => asked(command).pipe(Effect.provide(services));

    expect(yield* gathered({ command: "publish", environment: staging, version: null, accept_volume_loss: [] })).toEqual(removals(staging, false));
    expect(yield* gathered({ command: "admit", admit: "deploy", environment: staging, services: [] })).toEqual(removals(staging, false));
    expect(yield* gathered({ command: "admit", admit: "remove", environment: staging })).toEqual(removals(staging, true));
    // Naming Services still saves Working State, so the Store reviews what that Save drops (#3512).
    expect(yield* gathered({ command: "admit", admit: "deploy", environment: staging, services: ["web"] })).toEqual(removals(staging, false));
  }));

it.live("a push asks which Volumes a Save accepted losing, and without Servers to ask sends no evidence", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const pushed = (volumes: string[]) => Effect.gen(function* () {
      const read = vi.fn(async (_query: ConfigQuery) => ({ view: "consented", volumes }));
      const evidence = yield* systemVolumeEvidence(ORGANIZATION, asTestDouble<StoreRead>()(read)).pipe(Effect.provide(services));
      return { evidence, asked: read.mock.calls.map(([query]) => query) };
    });

    expect(yield* pushed([])).toEqual({ evidence: undefined, asked: [{ query: "consented" }] });
    expect(yield* pushed(["shop-production_vol-00000000-0000-4000-8000-000000000005"]))
      .toEqual({ evidence: undefined, asked: [{ query: "consented" }] });
  }));

it.live("a push asks the paired Servers about exactly the consented Volumes and hands what they hold to the Store; a sweep, which can't deploy, asks nothing", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const volume = "shop-production_vol-00000000-0000-4000-8000-000000000005";
    const held: VolumeObservation = { sought: [volume], held: [{ machine_id: machineId("0"), name: volume }], unanswered: [] };
    const observe = vi.fn(async (_connections: Connection[], _sought: string[]) => held);
    const store = yield* provided(cloudStore);
    const read = vi.spyOn(store, "read").mockResolvedValue({ view: "consented", volumes: [volume] });
    const system = vi.spyOn(store, "system").mockResolvedValue(asTestDouble<ConfigWritten>()({ written: "nothing" }));

    const told = (event: SystemEvent) => provided(storeSystem(ORGANIZATION, event).pipe(Effect.provideService(VolumeObserver, { observe })));
    const push: SystemEvent = {
      event: "branch_head", repository_id: 1, branch: "main", base: null, head: "0".repeat(40), changed: null,
    };

    yield* told(push);
    expect(read).toHaveBeenCalledExactlyOnceWith(ORGANIZATION, { query: "consented" });
    expect(observe).toHaveBeenCalledExactlyOnceWith([{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }], [volume]);
    expect(system).toHaveBeenCalledExactlyOnceWith(ORGANIZATION, push, expect.objectContaining({ volumes: held }));

    yield* told({ event: "sweep", now: 0 });
    expect(read).toHaveBeenCalledOnce();
    expect(observe).toHaveBeenCalledOnce();
    expect(system).toHaveBeenLastCalledWith(ORGANIZATION, { event: "sweep", now: 0 }, expect.not.objectContaining({ volumes: expect.anything() }));
  }));

it.live("a removal gathers for each Environment it takes off, and a shutdown for its own", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const gathered = (command: JsonObject) => asked(command).pipe(Effect.provide(services));
    const removing = (environment: string) => ({ query: "removals", environment: { project: "shop", environment }, remove: true });

    expect(yield* gathered({ command: "remove", project: "shop", environment: "staging" })).toEqual([removing("staging")]);
    expect(yield* gathered({ command: "remove", project: "shop" }))
      .toEqual([{ query: "environments", project: "shop" }, removing("production"), removing("staging")]);
    expect(yield* gathered({ command: "admit", admit: "shut_down", environment: staging })).toEqual([removing("staging")]);
  }));

it.live("a removal names the Servers that didn't answer, Volumes or not, and every one when none can be reached", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    const silent = machineId("1");
    const unanswered = (command: JsonObject, observe: (connections: Connection[], sought: string[]) => Promise<VolumeObservation>) =>
      provided(gatherUnanswered(ORGANIZATION, { operation: "write", command: raw(command) }, undefined)
        .pipe(Effect.provideService(VolumeObserver, { observe })));
    const remove = { command: "remove", project: "shop", environment: "staging" };
    const admit = { command: "admit", admit: "remove", environment: staging };
    const answers = (unanswered: VolumeObservation["unanswered"]) => async (): Promise<VolumeObservation> => ({ sought: [], held: [], unanswered });

    expect(yield* unanswered(remove, answers([silent]))).toBeUndefined();
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));

    expect(yield* unanswered(remove, answers([silent]))).toEqual([silent]);
    expect(yield* unanswered(admit, answers([]))).toEqual([]);
    expect(yield* unanswered(admit, () => Promise.reject(new RpcError({ code: "unavailable", message: "management connection timed out", details: null })))).toEqual([machineId("0")]);
    expect(yield* unanswered({ command: "admit", admit: "deploy", environment: staging, services: [] }, answers([silent])))
      .toBeUndefined();
  }));

it.live("a malformed removal probes no Server", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const observe = vi.fn(async (): Promise<VolumeObservation> => ({ sought: [], held: [], unanswered: [] }));
    const malformed: JsonObject[] = [
      { command: "remove" },
      { command: "remove", project: 5 },
      { command: "admit", admit: "remove", environment: 5 },
    ];
    for (const command of malformed) {
      expect(yield* provided(gatherUnanswered(ORGANIZATION, { operation: "write", command: raw(command) }, undefined)
        .pipe(Effect.provideService(VolumeObserver, { observe })))).toBeUndefined();
    }
    expect(observe).not.toHaveBeenCalled();
  }));

it.live("a removal whose Server connections fail to load says Cloud couldn't look, never that every Server answered", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    const silent = machineId("0");
    const admit = { command: "admit", admit: "remove", environment: staging };
    const observe = async (): Promise<VolumeObservation> => ({ sought: [], held: [], unanswered: [silent] });
    const unanswered = () => provided(gatherUnanswered(ORGANIZATION, { operation: "write", command: raw(admit) }, undefined)
      .pipe(Effect.provideService(VolumeObserver, { observe })));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const capability = (value: EncryptedSecretValue) => provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      yield* drizzle.update(organizationMachine).set({ encryptedCapability: value })
        .where(eq(organizationMachine.organizationId, ORGANIZATION));
    }));
    const [stored] = yield* provided(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      return yield* drizzle.select().from(organizationMachine).where(eq(organizationMachine.organizationId, ORGANIZATION));
    }));
    if (stored === undefined) throw new Error("the Server was enrolled");

    yield* capability({ ...stored.encryptedCapability, tag: "corrupt" });
    expect(yield* unanswered()).toBeUndefined();

    yield* capability(stored.encryptedCapability);
    expect(yield* unanswered()).toEqual([silent]);
  }));

it.live("a Publish naming no Environment gathers for the default one, and a malformed one gathers nothing", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const gathered = (command: JsonObject) => asked(command).pipe(Effect.provide(services));

    expect(yield* gathered({ command: "publish", version: null })).toEqual([{ query: "removals", environment: here, remove: false }]);
    expect(yield* gathered({ command: "publish", environment: { project: "shop" } })).toEqual([{ query: "removals", environment: { project: "shop", environment: null }, remove: false }]);
    for (const environment of [5, "staging", { project: 3 }, null]) {
      expect(yield* gathered({ command: "publish", environment, version: null, accept_volume_loss: [] })).toEqual([]);
    }
  }));

it.live("the Store still judges the raw Publish: a malformed Environment or an unknown field is refused, a well-formed one publishes", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
    yield* provided(seedStoreGitService(ORGANIZATION, ids));
    const publish = (command: JsonObject) => provided(callStore(ORGANIZATION, userId, { operation: "write", command: raw(command) }, { source: "dashboard" }, { approval: "not_required", permissions: permissionsOf("owner") }));

    const refused: JsonObject[] = [
      { command: "publish", environment: 5, version: null, accept_volume_loss: [] },
      { command: "publish", environment: here, version: null, accept_volume_loss: [], unknown: true },
    ];
    for (const command of refused) {
      expect(yield* publish(command)).toMatchObject({ ok: false, refusal: { code: "invalid_argument" } });
    }
    expect(yield* publish({ command: "publish", environment: here, version: null, accept_volume_loss: [] }))
      .toMatchObject({ ok: true, value: { written: "published", created: true } });
  }));
