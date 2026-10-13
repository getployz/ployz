import { createRequire } from "node:module";
import { it } from "@effect/vitest";
import type * as PloyzSdk from "@ployz/sdk";
import type { ConfigCommand, ConfigStore, CreateVolume, Mount } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect, Option } from "effect";
import { expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { failStoreDeploymentStorage } from "#/modules/config-store/store-deployment.server";
import { CloudStore, cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { lossTime, type PlacementRow } from "#/modules/volume-placement/volume-placement";
import { advancePlacement, type MemberInspector, type PlacementAdvance } from "#/modules/volume-placement/volume-placement.deploy.server";
import {
  discardPlacement, listPlacements, stageRestore, stampPlacements, writeAdmitted, type PlacementWrite,
} from "#/modules/volume-placement/volume-placement.server";
import type { Member } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const { RpcError } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "RpcError">;

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
    volumeId: id, environmentId: expect.any(String), mirrors: [], unreached: false, deploymentId: null,
    staged: {
      primary: member("web-1", "writer").machine.id, mirrors: [], deletes: [],
      seed: { sourceVolumeId: SOURCE, sourceName: "data", dockerVolume: `${namespace}_vol-${id}`, point },
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

/** A restore staged and then left behind: its Volume discarded from the Store, its row kept without a committed marker. */
const orphanedRestore = Effect.fn(function* () {
  const { services, store } = yield* deployedShop();
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  // A row staged before the marker existed, or whose marker write failed, then orphaned by a parallel Discard all.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(volumePlacement).set({ staged: sql`${volumePlacement.staged} #- '{seed,committed}'` });
  }).pipe(Effect.provide(services));
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "discard", environment: shop, path: null, version: null, target: "review" }));
  return { services, store, volumeId: staged.placement.volumeId };
});

/** A Deploy takes every placement staged, as one admitted with nothing more staged would. */
const stampStaged = (services: Effect.Success<ReturnType<typeof storeTestCloud>>, deployment: string) => Effect.gen(function* () {
  const staged = (yield* listPlacements(ORGANIZATION)).filter((row) => row.staged !== null).map((row) => row.volumeId);
  yield* stampPlacements(ORGANIZATION, staged, deployment);
}).pipe(Effect.provide(services));

const stampedDeploy = (services: Effect.Success<ReturnType<typeof storeTestCloud>>, store: ConfigStore, deployment: string) => Effect.gen(function* () {
  const admitted = yield* Effect.promise(() => admitDeploy(store, deployment));
  if (admitted.written !== "deployment") throw new Error("expected a Deployment");
  yield* stampStaged(services, admitted.id);
});

it.live("discards an orphaned restore's row once no request is staging it", () => Effect.gen(function* () {
  const { services, volumeId } = yield* orphanedRestore();
  expect(yield* discardPlacement(ORGANIZATION, null, volumeId, shop).pipe(Effect.provide(services))).toEqual({ ok: true, placement: null });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("drops an orphaned restore's row when a Deploy advances, so its source no longer waits", () => Effect.gen(function* () {
  const { services, store } = yield* orphanedRestore();
  const deployment = "00000000-0000-4000-8000-00000000d009";
  yield* stampedDeploy(services, store, deployment);
  expect(yield* advancePlacement(ORGANIZATION, deployment, null, () => Effect.succeed([member("web-1", "writer")]), () => Effect.succeed([]))
    .pipe(Effect.provide(services))).toEqual({ kind: "done" });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("drops a left-behind restore's row before asking the Servers, so a Server that can't take part doesn't fail the Deploy", () => Effect.gen(function* () {
  const { services, store } = yield* orphanedRestore();
  const deployment = "00000000-0000-4000-8000-00000000d00a";
  yield* stampedDeploy(services, store, deployment);
  expect(yield* advancePlacement(ORGANIZATION, deployment, null, () => Effect.succeed([member("web-1", "writer")]), () => Effect.succeed(["web-9"]))
    .pipe(Effect.provide(services))).toEqual({ kind: "done" });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

const byVolume: MemberInspector = (_, _environment, id) => Effect.succeed(id === SOURCE ? [member("web-1", "writer")] : []);

/** Lock waits in the database: a worker blocked on the admission's rows shows here. */
const lockWaits = (services: Effect.Success<ReturnType<typeof storeTestCloud>>) => Effect.runPromise(Effect.gen(function* () {
  const [row] = yield* (yield* Database).drizzle.execute<{ waiting: number }>(sql`select count(*)::int as waiting from pg_locks where not granted`, "objects");
  return (row?.waiting ?? 0) > 0;
}).pipe(Effect.provide(services)));

/** A Store that refuses every batch discard, as one unavailable when the failed Deploy is wound up. */
const refusingDiscards = (store: ConfigStore) => {
  const write: ConfigStore["write"] = async (organizationId, command, trusted, principal) => {
    if (command.command === "batch" && command.commands.some((step) => step.command === "discard")) {
      throw new RpcError({ code: "unavailable", message: "store down", details: null });
    }
    return store.write(organizationId, command, trusted, principal);
  };
  return Object.assign(Object.create(store) as ConfigStore, { write });
};

/**
 * A restore staged and stamped by a Deploy whose copy then failed, and the Store's record of that failure. With
 * `refuseDiscard`, the Store refuses to take the restore back then.
 */
const restoreThatFailed = Effect.fn(function* (refuseDiscard = false) {
  const { services, store } = yield* deployedShop();
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_deployment set status = 'applied' where id = '00000000-0000-4000-8000-00000000d006'`);
    for (const [kind, nodes, id] of [["service", "services", "id"], ["volume", "volumes", "resourceId"]] as const) {
      yield* drizzle.execute(sql`
        insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
        select environment_id, node->>${id}, organization_id, '00000000-0000-4000-8000-00000000d006', ${kind}, node::text
        from config_saved, lateral jsonb_array_elements(intent::jsonb->${nodes}) node`);
    }
  }).pipe(Effect.provide(services));
  const deployment = "00000000-0000-4000-8000-00000000d00c";
  const writer = () => Effect.succeed([member("web-1", "writer")]);
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, writer).pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  yield* stampedDeploy(services, store, deployment);

  const seeding = yield* advancePlacement(ORGANIZATION, deployment, null, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services));
  if (seeding.kind !== "wait") throw new Error("expected a run");
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update volume_run set state = 'failed', message = 'no space left', finished_at = now() where id = ${seeding.wait.runId}`);
  }).pipe(Effect.provide(services));
  const failed = yield* advancePlacement(ORGANIZATION, deployment, seeding.wait, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services));
  if (failed.kind !== "failed") throw new Error("expected the Deploy to fail");
  const { environment } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: deployment }));
  const wound = failStoreDeploymentStorage(ORGANIZATION, environment.id, deployment, "storage-run", failed.message, failed.restore);
  yield* (refuseDiscard ? wound.pipe(Effect.provideService(CloudStore, { open: Effect.succeed(refusingDiscards(store)) })) : wound).pipe(Effect.provide(services));
  return { services, store, deployment, volumeId: staged.placement.volumeId };
});

it.live("takes back a restore whose copy failed: the source keeps its mounts and the empty Volume is not staged", () => Effect.gen(function* () {
  const { services, store } = yield* restoreThatFailed();
  expect((yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop }))).volumes.map(({ name, mounts, change }) => ({ name, mounts, change })))
    .toEqual([{ name: "data", mounts: [{ service: "web", path: "/data" }], change: null }]);
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
    await Effect.runPromise(stampStaged(services, admitted.id));
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
  expect(refused).toBe(STARTING.refusal.message);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services)))
    .toMatchObject([{ volumeId: staged.placement.volumeId, staged: { seed: { sourceVolumeId: SOURCE } } }]);
}));

it.live("keeps a restore whose Store write hasn't committed when a Deploy advances past it", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const deployment = "00000000-0000-4000-8000-00000000d00b";
  let advanced: Effect.Success<ReturnType<typeof advancePlacement>> | null = null;
  let during: PlacementRow[] = [];
  const write: ConfigStore["write"] = async (organizationId, command, trusted, principal) => {
    if (command.command === "batch" && command.commands.some((step) => step.command === "create_volume")) {
      await Effect.runPromise(stampedDeploy(services, store, deployment));
      advanced = await Effect.runPromise(advancePlacement(ORGANIZATION, deployment, null, () => Effect.succeed([member("web-1", "writer")]), () => Effect.succeed([]))
        .pipe(Effect.provide(services)));
      during = await Effect.runPromise(listPlacements(ORGANIZATION).pipe(Effect.provide(services)));
    }
    return store.write(organizationId, command, trusted, principal);
  };
  const proxy = asTestDouble<ConfigStore>()({ read: (...args: Parameters<ConfigStore["read"]>) => store.read(...args), write });

  yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provideService(CloudStore, { open: Effect.succeed(proxy) }), Effect.provide(services));
  expect(advanced).toEqual({ kind: "done" });
  expect(during).toMatchObject([{ deploymentId: null, staged: { seed: { sourceVolumeId: SOURCE } } }]);
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

it.live("drops a discarded restore's row once a Deploy admitted between the Store discard and the row discard advances", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  yield* Effect.promise(() => store.write(ORGANIZATION, { command: "discard", environment: shop, path: null, version: null, target: "review" }));
  const deployment = "00000000-0000-4000-8000-00000000d008";
  const admitted = yield* Effect.promise(() => admitDeploy(store, deployment));
  if (admitted.written !== "deployment") throw new Error("expected a Deployment");
  yield* stampStaged(services, admitted.id);
  expect(yield* discardPlacement(ORGANIZATION, null, staged.placement.volumeId, shop).pipe(Effect.provide(services)))
    .toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });

  expect(yield* advancePlacement(ORGANIZATION, deployment, null, () => Effect.succeed([member("web-1", "writer")]), () => Effect.succeed([]))
    .pipe(Effect.provide(services))).toEqual({ kind: "done" });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("stages a restore when its committed marker can't be written", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const refuseUpdates = sql.raw(`
    create function refuse_placement_update() returns trigger language plpgsql as $$ begin raise exception 'placement updates refused'; end $$;
    create trigger refuse_placement_update before update on volume_placement for each row execute function refuse_placement_update();
  `);
  const write: ConfigStore["write"] = async (organizationId, command, trusted, principal) => {
    const written = await store.write(organizationId, command, trusted, principal);
    if (command.command === "batch") {
      await Effect.runPromise(Effect.gen(function* () { yield* (yield* Database).drizzle.execute(refuseUpdates); }).pipe(Effect.provide(services)));
    }
    return written;
  };
  const proxy = asTestDouble<ConfigStore>()({ read: (...args: Parameters<ConfigStore["read"]>) => store.read(...args), write });

  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.provideService(CloudStore, { open: Effect.succeed(proxy) }), Effect.provide(services));
  expect(staged).toMatchObject({ ok: true, placement: { staged: { seed: { sourceVolumeId: SOURCE } } } });
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toHaveLength(1);
}));

const STARTING = { ok: false, refusal: { code: "conflict", message: "A Deploy or restore is starting. Try again.", details: null } };

it.live("refuses a restore at once while a Deploy is admitted, holding nothing, and stages it once the Deploy is written", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const deployment = "00000000-0000-4000-8000-00000000d00d";
  const restore = () => stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, () => Effect.succeed([member("web-1", "writer")]))
    .pipe(Effect.timeoutOption("2 seconds"), Effect.map(Option.getOrNull), Effect.provide(services));
  const during: (PlacementWrite | null)[] = [];
  const write = (command: ConfigCommand) => storeTry(async () => {
    during.push(await Effect.runPromise(restore()), await Effect.runPromise(restore()));
    return store.write(ORGANIZATION, command, { ...acmeWeb(), servers: 1 });
  });

  yield* writeAdmitted(ORGANIZATION, {
    command: "admit", admit: "deploy", id: deployment, environment: shop, services: [], version: null, accept_volume_loss: [],
  }, write).pipe(Effect.provide(services));

  expect(during).toEqual([STARTING, STARTING]);
  const { nodes } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: deployment }));
  expect(nodes.flatMap((node) => node.type === "volume" ? [node.name] : [])).toEqual(["data"]);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
  expect(yield* restore()).toMatchObject({ ok: true, placement: { deploymentId: null, staged: { seed: { sourceVolumeId: SOURCE } } } });
}));

it.live("refuses to Redeploy a Deploy whose restore failed, and ships nothing", () => Effect.gen(function* () {
  const { services, store, deployment } = yield* restoreThatFailed();
  const writes: ConfigCommand[] = [];
  const refused = yield* writeAdmitted(ORGANIZATION, { command: "admit", admit: "retry", id: "00000000-0000-4000-8000-00000000d00e", deployment }, (command) => {
    writes.push(command);
    return storeTry(() => store.write(ORGANIZATION, command, { ...acmeWeb(), servers: 1 }));
  }).pipe(Effect.flip, Effect.provide(services));
  expect(refused).toMatchObject({ _tag: "StoreRefused", code: "conflict", message: "Restore failed. Restore again." });
  expect(writes).toEqual([]);
}));

/** A restore staged, then a Deploy the Store took with it whose stamp failed after the Store write committed. */
const unstampedRestore = Effect.fn(function* (deployment: string) {
  const { services, store } = yield* deployedShop();
  const writer = () => Effect.succeed([member("web-1", "writer")]);
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, writer).pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  const admitted = yield* writeAdmitted(ORGANIZATION, {
    command: "admit", admit: "deploy", id: deployment, environment: shop, services: [], version: null, accept_volume_loss: [],
  }, (command) => Effect.gen(function* () {
    yield* (yield* Database).drizzle.execute(sql`select 1/0`).pipe(Effect.ignore);
    return yield* storeTry(() => store.write(ORGANIZATION, command, { ...acmeWeb(), servers: 1 }));
  }), writer).pipe(Effect.provide(services));
  expect(admitted).toMatchObject({ written: "deployment", id: deployment });
  const name = (yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop })))
    .volumes.find((volume) => volume.id === staged.placement.volumeId)?.name;
  return { services, store, volumeId: staged.placement.volumeId, name };
});

it.live("fails a Deploy whose restore it didn't stamp before any run, and refuses to discard that restore until the Deploy ends", () => Effect.gen(function* () {
  const deployment = "00000000-0000-4000-8000-00000000d011";
  const { services, store, volumeId, name } = yield* unstampedRestore(deployment);

  expect(yield* discardPlacement(ORGANIZATION, null, volumeId, shop).pipe(Effect.provide(services)))
    .toMatchObject({ ok: false, refusal: { message: "A Deploy is moving this Volume. Wait for it to finish." } });
  const failed = yield* advancePlacement(ORGANIZATION, deployment, null, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services));
  expect(failed).toEqual({ kind: "failed", message: `${name}: the restore waits for the next Deploy.` });
  expect(yield* Effect.gen(function* () {
    return yield* (yield* Database).drizzle.execute(sql`select id from volume_run`);
  }).pipe(Effect.provide(services))).toMatchObject({ rowCount: 0 });

  const { environment } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: deployment }));
  yield* failStoreDeploymentStorage(ORGANIZATION, environment.id, deployment, "storage-run", failed.kind === "failed" ? failed.message : "").pipe(Effect.provide(services));
  expect(yield* discardPlacement(ORGANIZATION, null, volumeId, shop).pipe(Effect.provide(services))).toEqual({ ok: true, placement: null });
  expect((yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop }))).volumes.map((volume) => volume.name)).toEqual(["data"]);
}));

it.live("waits for the admission's stamp before judging a Deploy's restore, when the worker starts between the Store write and the stamp", () => Effect.gen(function* () {
  const { services, store } = yield* deployedShop();
  const writer = () => Effect.succeed([member("web-1", "writer")]);
  const staged = yield* stageRestore(ORGANIZATION, null, { volumeId: SOURCE, environment: shop, ...point }, writer).pipe(Effect.provide(services));
  if (!staged.ok) throw new Error(staged.refusal.message);
  const deployment = "00000000-0000-4000-8000-00000000d012";
  let advancing: Promise<PlacementAdvance> | undefined;
  const write = (command: ConfigCommand) => storeTry(async () => {
    const written = await store.write(ORGANIZATION, command, { ...acmeWeb(), servers: 1 });
    advancing = Effect.runPromise(advancePlacement(ORGANIZATION, deployment, null, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services)));
    let done = false;
    void advancing.finally(() => { done = true; });
    while (!done && !(await lockWaits(services))) await new Promise((resolve) => setTimeout(resolve, 10));
    return written;
  });

  yield* writeAdmitted(ORGANIZATION, {
    command: "admit", admit: "deploy", id: deployment, environment: shop, services: [], version: null, accept_volume_loss: [],
  }, write, writer).pipe(Effect.provide(services));

  expect(yield* Effect.promise(() => advancing ?? Promise.reject(new Error("no worker started"))))
    .toMatchObject({ kind: "wait", wait: { volumeId: staged.placement.volumeId, step: { change: { kind: "seed" } } } });
}));

it.live("fails one Deploy over a restore whose discard was refused, and takes it back so the next Deploy runs", () => Effect.gen(function* () {
  const { services, store, volumeId } = yield* restoreThatFailed(true);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toMatchObject([{ volumeId, staged: { seed: { sourceVolumeId: SOURCE } } }]);
  const name = (yield* listed(store, `data-${lossTime(new Date(at * 1000)).replace(":", "")}`))?.name;

  const second = "00000000-0000-4000-8000-00000000d013";
  yield* stampedDeploy(services, store, second);
  const failed = yield* advancePlacement(ORGANIZATION, second, null, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services));
  expect(failed).toMatchObject({ kind: "failed", message: `${name}: Restore failed. Restore again.` });
  if (failed.kind !== "failed") throw new Error("expected the Deploy to fail");
  const { environment } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: second }));
  yield* failStoreDeploymentStorage(ORGANIZATION, environment.id, second, "storage-run-2", failed.message, failed.restore).pipe(Effect.provide(services));

  const third = "00000000-0000-4000-8000-00000000d014";
  yield* stampedDeploy(services, store, third);
  expect(yield* advancePlacement(ORGANIZATION, third, null, byVolume, () => Effect.succeed([])).pipe(Effect.provide(services))).toEqual({ kind: "done" });
  const { nodes } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployment", id: third }));
  expect(nodes.flatMap((node) => node.type === "volume" ? [node.name] : [])).toEqual(["data"]);
  expect(yield* listPlacements(ORGANIZATION).pipe(Effect.provide(services))).toEqual([]);
}));

it.live("lets a Deployment Redeploy when another Deployment's restore failed", () => Effect.gen(function* () {
  const { services, store } = yield* restoreThatFailed();
  const writes: ConfigCommand[] = [];
  yield* writeAdmitted(ORGANIZATION, {
    command: "admit", admit: "retry", id: "00000000-0000-4000-8000-00000000d015", deployment: "00000000-0000-4000-8000-00000000d006",
  }, (command) => {
    writes.push(command);
    return storeTry(() => store.write(ORGANIZATION, command, { ...acmeWeb(), servers: 1 }));
  }).pipe(Effect.ignore, Effect.provide(services));
  expect(writes).toHaveLength(1);
}));
