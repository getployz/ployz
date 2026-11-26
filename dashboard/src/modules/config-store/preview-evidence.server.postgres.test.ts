import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import { closeUnheldPreviews } from "#/modules/config-store/preview-evidence.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, recordStoreSealingKey, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";

it.live("the sweep asks the Servers which previews they hold, even once no plan keeps previews, only once Cloud knows their Cluster", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
    yield* provided(seedStoreOrganization(ORGANIZATION));
    const store = yield* provided(cloudStore);
    const swept = vi.spyOn(store, "closeUnheldPreviews").mockResolvedValue({ closed: [], failed: [], unanswered: [], unverified: false, foreign: [] });

    yield* provided(closeUnheldPreviews(ORGANIZATION));
    expect(swept, "no Server to ask").not.toHaveBeenCalled();

    // Dialable, as an enrollment candidate is, but no Sealing Key recorded: whose Cluster it is can't be checked.
    yield* provided(enrollStoreServer(ORGANIZATION));
    yield* provided(closeUnheldPreviews(ORGANIZATION));
    expect(swept, "a Close could reach another Organization's Server").not.toHaveBeenCalled();

    // A Server that was down while its previews closed may still hold them.
    const sealingPublicKey = `${"P".repeat(43)}=`;
    yield* provided(recordStoreSealingKey(ORGANIZATION, sealingPublicKey));
    yield* provided(closeUnheldPreviews(ORGANIZATION));
    expect(swept).toHaveBeenCalledExactlyOnceWith(
      ORGANIZATION, [{ machine_id: machineId("0"), management: `ployz1:cloud:${machineId("0")}` }], { sealingPublicKey },
    );
  }), 60_000);
