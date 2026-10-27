import { it } from "@effect/vitest";
import type { ConfigStore, MachineId, WantedCopy } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { expect } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { findService, ownerOf } from "#/modules/config-store/store-lookup.server";
import type { CopyAsk } from "#/modules/volume-placement/mirror-sync.server";
import { refreshMirrors, stageOffServer } from "#/modules/volume-placement/volume-placement.deploy.server";
import type { CopyAnswer } from "#/modules/volume-run/volume-run.server";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000e001";
const SHOP = "00000000-0000-4000-8000-00000000e003";
const BLOG = "00000000-0000-4000-8000-00000000e013";
const web = (name: string) => name.padEnd(32, "0") as MachineId;
const members = [member("web-1", "writer"), member("web-2", "mirror")];

/** Projects `blog` and `shop`, each with Service web and Volume data deployed. */
const deployedProject = (store: ConfigStore, name: string, at: number) => Effect.promise(async () => {
  const id = (n: number) => `00000000-0000-4000-8000-00000000e${String(at + n).padStart(3, "0")}`;
  const here = { project: name, environment: null };
  await store.write(ORGANIZATION, { command: "create_project", id: id(2), name, default_environment: id(3) });
  await store.write(ORGANIZATION, { command: "create_service", id: id(5), environment: here, name: "web", image: "nginx:1" });
  await store.write(ORGANIZATION, {
    command: "create_volume", id: id(4), environment: here, name: "data", storage: { kind: "docker" }, mounts: [{ service: "web", path: "/data" }],
  });
  await store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: id(6), environment: here, services: [], version: null, accept_volume_loss: [],
  }, { ...acmeWeb(), servers: 1 });
  const { namespace } = await store.read(ORGANIZATION, { query: "namespace", environment: here });
  return `${namespace}_vol-${id(4)}`;
});

const setup = Effect.fn(function* () {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const blog = yield* deployedProject(store, "blog", 10);
  const shop = yield* deployedProject(store, "shop", 0);
  // Working State a newer Cloud wrote, as a rollback leaves it: the Store refuses to read blog.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_environment set working = '{"from_the_future":true}' where id = ${BLOG}`);
  }).pipe(Effect.orDie, Effect.provide(services));
  return { services, shop, blog };
});

const ask: CopyAsk = (_asker, _machine, _expect, want: WantedCopy) => Effect.succeed<CopyAnswer>(
  want.kind === "snapshot" ? { ok: true, copy: COPIES.writer } : { ok: true, copy: COPIES.pulling },
);

it.live("a Deploy refreshes its own mirrors while another Environment is unreadable", () => Effect.gen(function* () {
  const { services, shop, blog } = yield* setup();
  const copies = [shop, blog].flatMap((name) => [
    { machine_id: web("web-1"), name, role: "writer" as const }, { machine_id: web("web-2"), name, role: "slot" as const },
  ]);
  const refresh = (only: string | null) => refreshMirrors(ORGANIZATION, only, () => Effect.succeed(copies), () => Effect.succeed([]), () => Effect.succeed(members), ask)
    .pipe(Effect.provide(services));

  expect(yield* refresh(SHOP)).toHaveLength(1);
  // The organization-wide pass skips the Environment it can't read and refreshes the rest.
  expect(yield* refresh(null)).toHaveLength(1);
}));

it.live("finds a Service while another Environment is unreadable", () => Effect.gen(function* () {
  const { services } = yield* setup();
  expect(yield* ownerOf(ORGANIZATION, SHOP).pipe(Effect.provide(services))).toMatchObject({ project: "shop", environment: "production" });
  // Admitted but never run, so shop's web has no runtime yet: the lookup answers rather than failing over blog.
  expect(yield* findService(ORGANIZATION, SHOP, "web").pipe(Effect.provide(services))).toBeNull();
}));

it.live("a Drain refuses naming the Environment it can't read and the way out", () => Effect.gen(function* () {
  const { services, blog } = yield* setup();
  const copies = [{ machine_id: web("web-1"), name: blog, role: "writer" as const }];
  const refused = yield* stageOffServer(ORGANIZATION, web("web-1"), [web("web-2")], [], () => Effect.succeed(copies), () => Effect.succeed(members))
    .pipe(Effect.provide(services), Effect.flip);
  expect(refused).toMatchObject({
    _tag: "Conflict",
    userFacing: true,
    message: expect.stringMatching(/can't read blog\/production .*roll it forward/),
  });
}));
