import { it } from "@effect/vitest";
import type { ConfigStore, CreateVolume, Mount } from "@ployz/sdk";
import { Effect } from "effect";
import { expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore, cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { lossTime, type PlacementRow } from "#/modules/volume-placement/volume-placement";
import { discardPlacement, listPlacements, stageRestore, stampPlacements } from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000d001";
const SOURCE = "00000000-0000-4000-8000-00000000d004";
const shop = { project: "shop", environment: null };
const at = 1_790_000_000;
const point = { guid: "11", name: "ployz-1", at };

const admitDeploy = (store: ConfigStore, id: string) => store.write(ORGANIZATION, {
  command: "admit", admit: "deploy", id, environment: shop, services: [], version: null, accept_volume_loss: [],
}, { ...acmeWeb(), servers: 1 });

/** Organization ORGANIZATION with project shop, Service web (and any of `writers`), and Volume `data` deployed as `volume` says. */
const deployedShop = Effect.fn(function* (volume: Partial<CreateVolume> = {}, writers: string[] = []) {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  yield* Effect.promise(async () => {
    await store.write(ORGANIZATION, {
      command: "create_project", id: "00000000-0000-4000-8000-00000000d002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000d003",
    });
    for (const [index, name] of ["web", ...writers].entries()) {
      await store.write(ORGANIZATION, { command: "create_service", id: `00000000-0000-4000-8000-00000000d1${index}0`, environment: shop, name, image: "nginx:1" });
    }
    await store.write(ORGANIZATION, {
      command: "create_volume", id: SOURCE, environment: shop, name: "data", storage: { kind: "docker" }, mounts: [{ service: "web", path: "/data" }], ...volume,
    });
    await admitDeploy(store, "00000000-0000-4000-8000-00000000d006");
  });
  return { services, store };
});

const listed = (store: ConfigStore, name: string) => Effect.promise(async () =>
  (await store.read(ORGANIZATION, { query: "volumes", environment: shop })).volumes.find((volume) => volume.name === name));

it.live("stages a restore as one Store write and a seeded row, and discards both halves", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
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
      seed: { sourceVolumeId: SOURCE, sourceName: "data", dockerVolume: `${namespace}_vol-${id}`, point, committed: true },
    },
  });
  expect(yield* volumes()).toEqual([
    { name: "data", mounts: [], change: null },
    { name, mounts: [{ service: "web", path: "/data" }], change: "create" },
  ]);

  members = [member("web-2", "mirror")];
  expect(yield* restore()).toMatchObject({ ok: false, refusal: { message: "data has no primary answering to restore from." } });
  expect((yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).map((row) => row.volumeId)).toEqual([id]);

  expect(yield* discardPlacement(ORGANIZATION, null, id, shop).pipe(Effect.provide(services))).toEqual({ ok: true, placement: null });
  expect(yield* volumes()).toEqual([{ name: "data", mounts: [{ service: "web", path: "/data" }], change: null }]);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("discards a restore's row once the Store has discarded its Volume", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "discard", environment: shop, path: null, version: null, target: "review" }));

  expect(yield* discardPlacement(ORGANIZATION, null, staged.placement.volumeId, shop).pipe(Effect.provide(services))).toEqual({ ok: true, placement: null });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("names a restore within 63 characters, and past a Volume that already has its name", () => Effect.gen(function* () {
  const long = "d".repeat(63);
  const time = lossTime(new Date(at * 1000)).replace(":", "");
  const { services, store } = yield* deployedShop({ name: long });
  const restore = () => stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provide(services));
  const nameOf = (id: string) => Effect.promise(async () =>
    (await store.read(ORGANIZATION, { query: "volumes", environment: shop })).volumes.find((volume) => volume.id === id)?.name);

  const first = yield* restore();
  if (!first.ok) throw new Error(first.refusal.message);
  expect(yield* nameOf(first.placement.volumeId)).toBe(`${"d".repeat(58)}-${time}`);

  // An earlier restore holds that name: the same point restores again under a fresh one.
  const second = yield* restore();
  if (!second.ok) throw new Error(second.refusal.message);
  const name = yield* nameOf(second.placement.volumeId);
  expect(name).toMatch(/^d{58}-[a-z0-9]{4}$/u);
  expect(name).not.toBe(`${"d".repeat(58)}-${time}`);
}));

it.live("writes the restore's row before its Store write, so a Deploy admitted right after stamps it", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const deployment = "00000000-0000-4000-8000-00000000d007";
  let before: PlacementRow[] = [];
  const write: ConfigStore["write"] = async (organizationId, command, trusted, principal) => {
    if (command.command !== "batch") return store.write(organizationId, command, trusted, principal);
    before = await Effect.runPromise(listPlacements(ORGANIZATION).pipe(Effect.provide(services)));
    const written = await store.write(organizationId, command, trusted, principal);
    const admitted = await admitDeploy(store, deployment);
    if (admitted.written !== "deployment") throw new Error("expected a Deployment");
    await Effect.runPromise(stampPlacements(ORGANIZATION, admitted.environment_id, admitted.id).pipe(Effect.provide(services)));
    return written;
  };
  const proxy = asTestDouble<ConfigStore>()({ read: (...args: Parameters<ConfigStore["read"]>) => store.read(...args), write });

  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provideService(CloudStore, { open: Effect.succeed(proxy) }), Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  expect(before).toMatchObject([{ volumeId: staged.placement.volumeId, deploymentId: null, staged: { seed: { sourceVolumeId: SOURCE } } }]);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services)))
    .toMatchObject([{ volumeId: staged.placement.volumeId, deploymentId: deployment }]);
}));

it.live("refuses to discard a restore whose Store write hasn't committed, however long it has waited", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  let refused = "";
  const write: ConfigStore["write"] = async (organizationId, command, trusted, principal) => {
    if (command.command === "batch" && command.commands.some((step) => step.command === "create_volume")) {
      const [row] = await Effect.runPromise(listPlacements(ORGANIZATION).pipe(Effect.provide(services)));
      const later = vi.spyOn(Date, "now").mockReturnValue(Date.now() + 3_600_000);
      const discarded = await Effect.runPromise(discardPlacement(ORGANIZATION, null, row?.volumeId ?? "", shop).pipe(Effect.provide(services)));
      later.mockRestore();
      refused = discarded.ok ? "" : discarded.refusal.message;
    }
    return store.write(organizationId, command, trusted, principal);
  };
  const proxy = asTestDouble<ConfigStore>()({ read: (...args: Parameters<ConfigStore["read"]>) => store.read(...args), write });

  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provideService(CloudStore, { open: Effect.succeed(proxy) }), Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  expect(refused).toBe("This restore is still being staged. Try again in a minute.");
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services)))
    .toMatchObject([{ volumeId: staged.placement.volumeId, staged: { seed: { sourceVolumeId: SOURCE } } }]);
}));

it.live("restores onto the source's storage, locked, with the source's shared writes", () => Effect.gen(function* () {
  const managed = { kind: "provisioned", maximumBytes: 5_000_000_000 } as const;
  const mounts: Mount[] = [{ service: "web", path: "/data" }, { service: "worker", path: "/data" }];
  const { services, store } = yield* deployedShop({ storage: managed, shared_writes: true, mounts }, ["worker"]);
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  const name = `data-${lossTime(new Date(at * 1000)).replace(":", "")}`;
  expect(yield* listed(store, name)).toMatchObject({ storage: managed, storage_locked: true, shared_writes: true, mounts, change: "create" });

  const resized = yield* storeTry(() => store.write(ORGANIZATION, {
    command: "set_volume_storage", environment: shop, volume: name, storage: { kind: "provisioned", maximumBytes: 10_000_000_000 },
  })).pipe(Effect.flip);
  expect(resized).toMatchObject({ details: { storage_locked: true } });
}));

