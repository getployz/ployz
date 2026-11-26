import { it } from "@effect/vitest";
import { Effect, Exit } from "effect";
import { expect } from "vitest";
import { closeUnheldPreviews } from "#/modules/config-store/preview-evidence.server";
import { enrollStoreServer, recordStoreSealingKey, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000c001";

it.live("the sweep asks the Servers which previews they hold even once no plan keeps previews", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
    const quiet = yield* closeUnheldPreviews(ORGANIZATION).pipe(Effect.provide(services), Effect.exit);
    expect(Exit.isSuccess(quiet), "no Server to ask").toBe(true);

    // A Server that was down while its previews closed may still hold them. The fixture Server answers nothing, so
    // asking it fails, where not asking succeeds.
    yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));
    yield* recordStoreSealingKey(ORGANIZATION, `${"P".repeat(43)}=`).pipe(Effect.provide(services));
    const asked = yield* closeUnheldPreviews(ORGANIZATION).pipe(Effect.provide(services), Effect.exit);
    expect(Exit.isFailure(asked), "the sweep dialed the Server").toBe(true);
  }), 60_000);

it.live("the sweep sends no Close to a Server before Cloud knows its Cluster", () =>
  Effect.gen(function* () {
    const services = yield* storeTestCloud();
    yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
    // Dialable, as an enrollment candidate is, but no Sealing Key recorded: whose Cluster it is can't be checked.
    yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));
    const swept = yield* closeUnheldPreviews(ORGANIZATION).pipe(Effect.provide(services), Effect.exit);
    expect(Exit.isSuccess(swept), "the sweep dialed no Server").toBe(true);
  }), 60_000);
