import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { listStrayNamespaces, loadNamespaceDataLoss } from "./namespace-cleanup.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e001";

it.live("offers to remove only Namespaces no Environment owns, and refuses an owned one", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  const userId = yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_project", id: "00000000-0000-4000-8000-00000000e002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000e003",
  }));
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: { project: "shop", environment: null } }));
  const actor = { userId };

  const strays = yield* listStrayNamespaces(actor, { organizationSlug: "shop", namespaces: [namespace, "left-behind", "ployz-system"] })
    .pipe(Effect.provide(services));
  expect(strays).toEqual(["left-behind"]);
  const refused = yield* loadNamespaceDataLoss(actor, { organizationSlug: "shop", namespace }).pipe(Effect.scoped, Effect.provide(services), Effect.flip);
  expect(refused).toMatchObject({ _tag: "Conflict", message: expect.stringContaining("belongs to an Environment") });
}), 60_000);
