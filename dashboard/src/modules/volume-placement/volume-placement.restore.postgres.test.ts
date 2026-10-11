import { it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { lossTime } from "#/modules/volume-placement/volume-placement";
import { discardPlacement, listPlacements, stageRestore } from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d001";
const SOURCE = "00000000-0000-4000-8000-00000000d004";
const shop = { project: "shop", environment: null };
const at = 1_790_000_000;
const point = { guid: "11", name: "ployz-1", at };

it.live("stages a restore as one Store write and a seeded row, and discards both halves", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  yield* Effect.promise(async () => {
    await store.write(ORGANIZATION, {
      command: "create_project", id: "00000000-0000-4000-8000-00000000d002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000d003",
    });
    await store.write(ORGANIZATION, { command: "create_service", id: "00000000-0000-4000-8000-00000000d005", environment: shop, name: "web", image: "nginx:1" });
    await store.write(ORGANIZATION, {
      command: "create_volume", id: SOURCE, environment: shop, name: "data", storage: { kind: "docker" }, mounts: [{ service: "web", path: "/data" }],
    });
    await store.write(ORGANIZATION, {
      command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000d006", environment: shop, services: [], version: null, accept_volume_loss: [],
    }, { ...acmeWeb(), servers: 1 });
  });
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: shop }));
  const volumes = () => Effect.promise(async () =>
    (await store.read(ORGANIZATION, { query: "volumes", environment: shop })).volumes.map(({ name, mounts, change }) => ({ name, mounts, change })));
  let members: Member[] = [member("web-1", "writer"), member("web-2", "mirror")];
  const restore = () => stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed(members))
    .pipe(Effect.provide(services));
  const name = `data-${lossTime(new Date(at * 1000)).replace(":", "")}`;

  const staged = yield* restore();
  if (!staged.ok) throw new Error(staged.refusal.message);
  const id = staged.placement.volumeId;
  expect(staged.placement).toEqual({
    volumeId: id, environmentId: expect.any(String), mirrors: [], deploymentId: null,
    staged: {
      primary: member("web-1", "writer").machine.id, mirrors: [], deletes: [],
      seed: { sourceVolumeId: SOURCE, sourceName: "data", dockerVolume: `${namespace}_vol-${id}`, point },
    },
  });
  expect(yield* volumes()).toEqual([
    { name: "data", mounts: [], change: null },
    { name, mounts: [{ service: "web", path: "/data" }], change: "create" },
  ]);

  // The Store refuses a second Volume by the same name: nothing is written, and no row records it.
  const again = yield* restore();
  expect(again).toMatchObject({ ok: false, refusal: { code: expect.any(String) } });
  members = [member("web-2", "mirror")];
  expect(yield* restore()).toMatchObject({ ok: false, refusal: { message: "data has no primary answering to restore from." } });
  expect((yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).map((row) => row.volumeId)).toEqual([id]);

  expect(yield* discardPlacement(ORGANIZATION, null, id, shop).pipe(Effect.provide(services))).toEqual({ ok: true, placement: null });
  expect(yield* volumes()).toEqual([{ name: "data", mounts: [{ service: "web", path: "/data" }], change: null }]);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));
