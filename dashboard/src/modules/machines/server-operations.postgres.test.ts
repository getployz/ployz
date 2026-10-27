import { it } from "@effect/vitest";
import type { MachineId, RuntimeWatchView } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { expect } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { createVolumeCommand } from "#/modules/config-store/store-volumes";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { dockerVolumeName } from "#/modules/volume-run/volume-run";
import { Database } from "#/server/database.server";
import { acmeWeb, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";
import { nameLostVolumes, planRemove } from "./server-operations.server";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f001";
const fra1 = "f".repeat(32) as MachineId;

const cluster = Layer.succeed(OrganizationRuntime, {
  cancel: () => Effect.void,
  open: () => Effect.succeed({
    status: "connected" as const,
    connected: asTestDouble<PloyzSession>()({
      watchFirstFrame: () => Effect.succeed(asTestDouble<RuntimeWatchView>()({
        machines: [{ machine: { id: fra1, name: "fra-1" } }],
        services: [],
      })),
    }),
  }),
});

it.live("a Server removal's ask names each Volume it deletes as Ployz names it, not by its Docker name", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const shop = { project: "shop", environment: null };
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_project", id: "00000000-0000-4000-8000-00000000f002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000f003",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION,
    createVolumeCommand("00000000-0000-4000-8000-00000000f004", shop, "cache-data", { kind: "docker" })));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_service", id: "00000000-0000-4000-8000-00000000f005", environment: shop, name: "web", image: "nginx:1",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000f006", environment: shop, services: [], version: null, accept_volume_loss: [],
  }, { ...acmeWeb(), servers: 1 }));
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: shop }));
  const { volumes } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop }));
  const docker = dockerVolumeName(namespace, volumes[0]?.id ?? "");

  const plan = yield* planRemove(ORGANIZATION, fra1, [{ kind: "docker_volume", id: { machine_id: fra1, name: docker } }])
    .pipe(Effect.provide(cluster), Effect.provide(services));

  expect(plan?.name).toBe("fra-1");
  expect(plan?.effects).toEqual([
    { kind: "removes_server", name: "fra-1", node: fra1, path: `servers/${fra1}` },
    { kind: "deletes_volume", name: "cache-data", node: `${fra1}/${docker}`, path: `volumes/${fra1}/${docker}` },
  ]);
}), 60_000);

it.live("names each Volume a removal loses by its Volume and Environment, and an anonymous Docker volume as anonymous", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const shop = { project: "shop", environment: null };
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_project", id: "00000000-0000-4000-8000-00000000f002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000f003",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION,
    createVolumeCommand("00000000-0000-4000-8000-00000000f004", shop, "pg-data", { kind: "docker" })));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_service", id: "00000000-0000-4000-8000-00000000f005", environment: shop, name: "db", image: "postgres:17",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000f006", environment: shop, services: [], version: null, accept_volume_loss: [],
  }, { ...acmeWeb(), servers: 1 }));
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: shop }));
  const { volumes } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop }));
  const docker = dockerVolumeName(namespace, volumes[0]?.id ?? "");
  const anonymous = "68ab0a2eb28fcf4457dd147e161af2ee0ea9ec78823d371133e768d5d8f5f4a0";
  const lost = [docker, anonymous, "my-cache"].map((name) => ({ kind: "docker_volume" as const, id: { machine_id: fra1, name } }));

  expect(yield* nameLostVolumes(ORGANIZATION, lost).pipe(Effect.provide(services))).toEqual([
    { docker, name: "pg-data", detail: "shop/production" },
    { docker: anonymous, name: "Anonymous volume", detail: "68ab0a2eb28f" },
    { docker: "my-cache", name: "my-cache" },
  ]);
}), 60_000);

it.live("a Server removal refuses, naming the Environment the Store can't read and the way out", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const shop = { project: "shop", environment: null };
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_project", id: "00000000-0000-4000-8000-00000000f002", name: "shop", default_environment: "00000000-0000-4000-8000-00000000f003",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION,
    createVolumeCommand("00000000-0000-4000-8000-00000000f004", shop, "cache-data", { kind: "docker" })));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "create_service", id: "00000000-0000-4000-8000-00000000f005", environment: shop, name: "web", image: "nginx:1",
  }));
  yield* Effect.promise(() => store.write(ORGANIZATION, {
    command: "admit", admit: "deploy", id: "00000000-0000-4000-8000-00000000f006", environment: shop, services: [], version: null, accept_volume_loss: [],
  }, { ...acmeWeb(), servers: 1 }));
  const { namespace } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "namespace", environment: shop }));
  const { volumes } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment: shop }));
  const docker = dockerVolumeName(namespace, volumes[0]?.id ?? "");
  // Working State the Store can't read back, as a rollback or a damaged row leaves it.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update config_environment set working = '{"from_the_future":true}' where id = '00000000-0000-4000-8000-00000000f003'`);
  }).pipe(Effect.orDie, Effect.provide(services));

  const refused = yield* planRemove(ORGANIZATION, fra1, [{ kind: "docker_volume", id: { machine_id: fra1, name: docker } }])
    .pipe(Effect.provide(cluster), Effect.provide(services), Effect.flip);

  expect(refused).toMatchObject({ _tag: "Conflict", userFacing: true, message: expect.stringContaining("can't read shop/production") });
}), 60_000);
