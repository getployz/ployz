import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import { retireOldCopies } from "#/modules/config-store/retire-old.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, recordStoreSealingKey, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000af02";
const DEPLOYMENT = "00000000-0000-4000-8000-00000000af03";

it.live("a cleanup of a Deployment the Store no longer has, gone with its Environment, retires nothing and ends", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const requested = { organizationId: ORGANIZATION, environmentId: "pr-1", deploymentId: DEPLOYMENT, number: 2 };
    expect(yield* provided(retireOldCopies(requested))).toEqual({ left: [], kept: [] });
  }), 60_000);

it.live("a cleanup retires through a Server of the pairing's Cluster once its Sealing Key is known, as a Deploy runs", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(program: Effect.Effect<A, E, R>) => program.pipe(Effect.provide(services));
    yield* provided(Effect.all([seedStoreOrganization(ORGANIZATION), enrollStoreServer(ORGANIZATION)]));
    const retireOld = vi.spyOn(yield* provided(cloudStore), "retireOld").mockResolvedValue({ left: [], kept: [] });
    const requested = { organizationId: ORGANIZATION, environmentId: "pr-1", deploymentId: DEPLOYMENT, number: 2 };
    const servers = [{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }];

    yield* provided(retireOldCopies(requested));
    expect(retireOld).toHaveBeenLastCalledWith(ORGANIZATION, DEPLOYMENT, servers, null);

    const pairingKey = `${"P".repeat(43)}=`;
    yield* provided(recordStoreSealingKey(ORGANIZATION, pairingKey));
    yield* provided(retireOldCopies(requested));
    expect(retireOld).toHaveBeenLastCalledWith(ORGANIZATION, DEPLOYMENT, servers, { sealingPublicKey: pairingKey });
  }), 60_000);
