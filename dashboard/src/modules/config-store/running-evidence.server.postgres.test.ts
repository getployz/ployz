import { it } from "@effect/vitest";
import type { ConfigCommand, Connection, RunningObservation } from "@ployz/sdk";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import type { JsonObject } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import { gatherRunning, RunningObserver } from "#/modules/config-store/running-evidence.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e801";
const staging = { project: "shop", environment: "staging" };
const raw = asTestDouble<ConfigCommand>();
const seen: RunningObservation = { services: ["shop-staging/web", "shop-staging/extra2"], unanswered: [] };

it.live("only a full Deploy that prunes, or a retry, asks the Servers what runs, and hands what they said to the Store", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const observe = vi.fn(async (_connections: Connection[]) => seen);
    const gathered = (command: JsonObject) => provided(
      gatherRunning(ORGANIZATION, { operation: "write", command: raw(command) }).pipe(Effect.provideService(RunningObserver, { observe })),
    );
    const deploy = { command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000e802", environment: staging, version: null, accept_volume_loss: [] };

    expect(yield* gathered({ ...deploy, services: [] })).toEqual(seen);
    expect(observe).toHaveBeenCalledExactlyOnceWith([{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }]);
    expect(yield* gathered(deploy)).toEqual(seen);
    expect(yield* gathered({ ...deploy, services: [], prune: true })).toEqual(seen);
    expect(yield* gathered({ command: "admit", admit: "retry", id: deploy.id, deployment: deploy.id, rolls_back: null, version: null, accept_volume_loss: [] })).toEqual(seen);
    expect(observe).toHaveBeenCalledTimes(4);

    expect(yield* gathered({ ...deploy, services: [], no_prune: true })).toBeUndefined();
    expect(yield* gathered({ ...deploy, services: ["web"] })).toBeUndefined();
    expect(yield* gathered({ command: "publish", environment: staging, version: null, accept_volume_loss: [] })).toBeUndefined();
    expect(observe).toHaveBeenCalledTimes(4);
  }));

it.live("without Servers to ask, a pruning Deploy carries no evidence, so the Store can't read a prune as removing nothing", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const observe = vi.fn(async (_connections: Connection[]) => seen);
    const command = raw({ command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000e803", environment: staging, services: [], version: null, accept_volume_loss: [] });
    const gathered = yield* gatherRunning(ORGANIZATION, { operation: "write", command }).pipe(
      Effect.provideService(RunningObserver, { observe }),
      Effect.provide(services),
    );
    expect(gathered).toBeUndefined();
    expect(observe).not.toHaveBeenCalled();
  }));
