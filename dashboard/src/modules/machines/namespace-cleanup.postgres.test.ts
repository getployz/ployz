import { it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { operationsOn } from "#/modules/operations/operation-history.server";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { listStrayNamespaces, loadNamespaceDataLoss } from "./namespace-cleanup.server";
import { namespaceCleanup } from "./tables";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e001";

it.live("offers to remove only Namespaces no Environment owns, and refuses an owned one", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const shop = { project: "shop", environment: null };
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_project", id: "00000000-0000-4000-8000-00000000e002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000e003",
  }));
  // Admission fixes the Environment's Namespace: from then on it owns it.
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_service", id: "00000000-0000-4000-8000-00000000e004", environment: shop, name: "web", image: "nginx:1",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000e005", environment: shop, services: [], version: null, accept_volume_loss: [],
  }, { ...acmeWeb(), servers: 1 }));
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: shop }));
  const actor = { userId };

  const strays = yield* listStrayNamespaces(actor, { organizationSlug: "shop", namespaces: [namespace, "left-behind", "ployz-system"] })
    .pipe(Effect.provide(services));
  expect(strays).toEqual(["left-behind"]);
  const refused = yield* loadNamespaceDataLoss(actor, { organizationSlug: "shop", namespace }).pipe(Effect.scoped, Effect.provide(services), Effect.flip);
  expect(refused).toMatchObject({ _tag: "Conflict", message: expect.stringContaining("belongs to an Environment") });
}), 60_000);

// Catches a clean that never started or was lost reaching the agent with no reason.
it.live("says why a clean ended without the Engine's words", () => Effect.gen(function* () {
  const services = yield* Layer.build(yield* storeTestCloud());
  const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const { drizzle } = yield* Database.pipe(Effect.provide(services));
  const NOT_STARTED = "00000000-0000-4000-8000-00000000e011";
  const LOST = "00000000-0000-4000-8000-00000000e012";
  const endedAt = new Date();
  const row = { organizationId: ORGANIZATION, namespace: "left-behind", requestedByUserId: userId, confirmDataLoss: [], endedAt };
  yield* drizzle.insert(namespaceCleanup).values([
    { ...row, id: NOT_STARTED, state: "failed", endCode: "not_started" },
    { ...row, id: LOST, state: "unknown", endCode: "lost", inngestRunId: "run-lost", startedAt: endedAt },
  ]);
  const read = (attempt: string) =>
    operationsOn(ORGANIZATION, { kind: "leftover", namespace: "left-behind" }, attempt, 1).pipe(Effect.provide(services));

  expect(yield* read(NOT_STARTED)).toEqual([expect.objectContaining({ state: "failed", reason: "The clean never started." })]);
  expect(yield* read(LOST)).toEqual([
    expect.objectContaining({ state: "unknown", reason: "Cloud lost track of the clean; some Volumes may be gone." }),
  ]);
}), 60_000);
