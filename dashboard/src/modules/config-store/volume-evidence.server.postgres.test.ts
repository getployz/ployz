import { it } from "@effect/vitest";
import type { ConfigCommand, ConfigQuery } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { expect, vi } from "vitest";
import type { JsonObject, JsonValue } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { callStore } from "#/modules/config-store/config-store.server";
import type { StoreRead } from "#/modules/config-store/store.contract";
import { gatherVolumeEvidence } from "#/modules/config-store/volume-evidence.server";
import { seedStoreGitService, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e701";
const ids = { project: "00000000-0000-4000-8000-00000000e702", environment: "00000000-0000-4000-8000-00000000e703", service: "00000000-0000-4000-8000-00000000e704" };
const staging = { project: "shop", environment: "staging" };
const here = { project: null, environment: null };

/** A command as it arrives raw from a caller, before the Store's own decoding judges it. */
const raw = asTestDouble<ConfigCommand>();

/** Which removals query, if any, gathering evidence for `command` asks the Store. */
const asked = (command: JsonObject) => Effect.gen(function* () {
  const read = vi.fn(async (_query: ConfigQuery) => ({ view: "removals", volumes: [] }));
  const evidence = yield* gatherVolumeEvidence(ORGANIZATION, { operation: "write", command: raw(command) }, asTestDouble<StoreRead>()(read));
  expect(evidence).toBeUndefined();
  return read.mock.calls.map(([query]) => query);
});

const cloud = Effect.fn(function* () {
  const layer = yield* storeTestCloud();
  return yield* Layer.build(layer);
});

it.live("a Publish gathers what the Servers hold of the Volumes it removes, as a full Deploy does", () =>
  Effect.gen(function* () {
    const services = yield* cloud();
    const removals = (environment: JsonValue, remove: boolean) => [{ query: "removals", environment, remove }];
    const gathered = (command: JsonObject) => asked(command).pipe(Effect.provide(services));

    expect(yield* gathered({ command: "publish", environment: staging, version: null, accept_volume_loss: [] })).toEqual(removals(staging, false));
    expect(yield* gathered({ command: "admit", admit: "deploy", environment: staging, services: [] })).toEqual(removals(staging, false));
    expect(yield* gathered({ command: "admit", admit: "remove", environment: staging })).toEqual(removals(staging, true));
    expect(yield* gathered({ command: "admit", admit: "deploy", environment: staging, services: ["web"] })).toEqual([]);
  }));

it.live("a Publish naming no Environment gathers for the default one, and a malformed one gathers nothing", () =>
  Effect.gen(function* () {
    const services = yield* cloud();
    const gathered = (command: JsonObject) => asked(command).pipe(Effect.provide(services));

    expect(yield* gathered({ command: "publish", version: null })).toEqual([{ query: "removals", environment: here, remove: false }]);
    expect(yield* gathered({ command: "publish", environment: { project: "shop" } })).toEqual([{ query: "removals", environment: { project: "shop", environment: null }, remove: false }]);
    for (const environment of [5, "staging", { project: 3 }, null]) {
      expect(yield* gathered({ command: "publish", environment, version: null, accept_volume_loss: [] })).toEqual([]);
    }
  }));

it.live("the Store still judges the raw Publish: a malformed Environment or an unknown field is refused, a well-formed one publishes", () =>
  Effect.gen(function* () {
    const services = yield* cloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    const userId = yield* provided(seedStoreOrganization(ORGANIZATION));
    yield* provided(seedStoreGitService(ORGANIZATION, ids));
    const publish = (command: JsonObject) => provided(callStore(ORGANIZATION, userId, { operation: "write", command: raw(command) }));

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
