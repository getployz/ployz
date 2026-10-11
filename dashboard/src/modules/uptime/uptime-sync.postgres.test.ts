import { randomUUID } from "node:crypto";
import { it } from "@effect/vitest";
import { InngestTestEngine } from "@inngest/test";
import type { JsonValue, SavedServiceIntent } from "@ployz/sdk";
import { sql, type SQL } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect } from "vitest";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";
import { controlPlaneStatements } from "#/modules/uptime/incident-fence";
import { syncUptimeChecks } from "#/modules/uptime/uptime-sync.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { Database } from "#/server/database.server";
import { seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000b001";
const PROJECT = "00000000-0000-4000-8000-00000000b002";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000b003";
const SERVICE = "00000000-0000-4000-8000-00000000b004";
const DEPLOYED = "00000000-0000-4000-8000-00000000b101";
const CLUSTER_DOMAIN = "acme.ployz.test";
const here = { project: null, environment: null };

type Healthcheck = { type: "none" } | { type: "http"; path: string; timeoutSeconds: number };
type CheckRow = {
  id: string; name: string; url: string; source: string; interval_seconds: number; timeout_ms: number; expect: string;
  enabled: boolean; revision: number; environment_id: string; service_id: string; updated_at: string;
};

/**
 * Cloud with the Store open on a fresh database: Service `web` in Environment ENVIRONMENT, and Deployment DEPLOYED
 * admitted with the Cluster Domain frozen. `apply` writes the Service's Applied State row as the Store's `advance` does.
 */
const setup = Effect.fn(function* () {
  const services = yield* Layer.build(yield* storeTestCloud());
  yield* seedStoreOrganization(ORGANIZATION).pipe(Effect.provide(services));
  const store = yield* cloudStore.pipe(Effect.provide(services));
  const write = (command: Parameters<typeof store.write>[1]) => Effect.promise(() => store.write(ORGANIZATION, command));
  yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
  yield* write({ command: "create_service", id: SERVICE, environment: here, name: "web", image: "nginx:1" });
  yield* write({ command: "admit", admit: "deploy", id: DEPLOYED, environment: here, services: [], version: null, accept_volume_loss: [] });

  const query = <Row extends Record<string, JsonValue>>(statement: SQL) => Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.execute<Row>(statement, "objects");
  }).pipe(Effect.orDie, Effect.provide(services));
  yield* query(sql`update config_deployment set cluster_domain = ${CLUSTER_DOMAIN} where id = ${DEPLOYED}`);
  const [introduced] = yield* query<{ node: string }>(
    sql`select node from config_node_introduction where environment_id = ${ENVIRONMENT} and node_id = ${SERVICE}`);
  if (introduced === undefined) throw new Error("the Store introduced no Service");
  const saved = JSON.parse(introduced.node) as SavedServiceIntent;

  const apply = (config: { prefixes: string[]; healthcheck: Healthcheck }) => {
    const node = JSON.stringify({
      ...saved,
      config: { ...saved.config, healthcheck: config.healthcheck, managedHostnames: config.prefixes.map((prefix) => ({ prefix, targetPort: null })) },
    });
    return query(sql`
      insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
      values (${ENVIRONMENT}, ${SERVICE}, ${ORGANIZATION}, ${DEPLOYED}, 'service', ${node})
      on conflict (environment_id, node_id) do update set deployment_id = excluded.deployment_id, node = excluded.node`);
  };
  const sync = syncUptimeChecks(ORGANIZATION, ENVIRONMENT).pipe(Effect.provide(services));
  const checks = query<CheckRow>(sql`select *, updated_at::text from uptime_check order by created_at`);
  const runner: Parameters<typeof createRunStoreDeployment>[1] =
    makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
  const worker = createRunStoreDeployment(new Inngest({ id: "uptime-sync-test" }), runner);
  /** One delivery of DEPLOYED's admission; `applied` stands in for a runner that applied it. */
  const deliver = (applied: boolean) => Effect.promise(() => new InngestTestEngine({
    function: worker,
    events: [{ name: configDeploymentAdmittedEvent, data: { organizationId: ORGANIZATION, environmentId: ENVIRONMENT, deploymentId: DEPLOYED } }],
    steps: applied
      ? [
        { id: "run-deployment", handler: () => ({ ran: { id: DEPLOYED, status: "applied", remove: false } }) },
        { id: "delete-orphan-mirrors", handler: () => null },
      ]
      : [],
  }).execute());
  return { query, apply, sync, checks, deliver };
});

const none: Healthcheck = { type: "none" };
const http = (path: string): Healthcheck => ({ type: "http", path, timeoutSeconds: 5 });

it.live("an applied Deployment creates one check per generated domain, with the table's defaults", () => Effect.gen(function* () {
  const { apply, checks, deliver } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: http("/healthz") });
  yield* deliver(true);
  expect(yield* checks).toEqual([expect.objectContaining({
    name: "web", url: `https://web-shop.${CLUSTER_DOMAIN}/healthz`, source: "service", environment_id: ENVIRONMENT, service_id: SERVICE,
    interval_seconds: 300, timeout_ms: 10000, expect: "below_400", enabled: true, revision: 1,
  })]);
}));

it.live("a Service without an HTTP healthcheck is checked at /, and one without a generated domain is not checked", () => Effect.gen(function* () {
  const { apply, sync, checks } = yield* setup();
  yield* apply({ prefixes: [], healthcheck: http("/healthz") });
  yield* sync;
  expect(yield* checks).toEqual([]);
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  expect((yield* checks).map((check) => check.url)).toEqual([`https://web-shop.${CLUSTER_DOMAIN}/`]);
}));

it.live("an unchanged URL writes nothing", () => Effect.gen(function* () {
  const { apply, sync, checks } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  const before = yield* checks;
  yield* sync;
  expect(yield* checks).toEqual(before);
  expect(before).toEqual([expect.objectContaining({ revision: 1 })]);
}));

it.live("a URL change bumps the revision and closes the open incident as check_changed with no alert", () => Effect.gen(function* () {
  const { apply, sync, checks, query } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  const [created] = yield* checks;
  if (created === undefined) throw new Error("no check");
  const evidence = `'${JSON.stringify({ observations: [] })}'`;
  const opened = randomUUID();
  // The control plane's own open, at revision 1, with its literals in place of the Rust checker's parameters.
  const literals = [`'${created.id}'`, "1", `'${opened}'`, "now()", evidence, evidence];
  yield* query(sql.raw(controlPlaneStatements.open.replace(/\$(\d)/g, (_, n: string) => literals[Number(n) - 1] ?? "")));

  yield* apply({ prefixes: ["web-shop"], healthcheck: http("/up") });
  yield* sync;
  expect(yield* checks).toEqual([expect.objectContaining({ id: created.id, url: `https://web-shop.${CLUSTER_DOMAIN}/up`, revision: 2 })]);
  expect(yield* query(sql`select id, closed_by from uptime_incident`)).toEqual([{ id: opened, closed_by: { kind: "check_changed" } }]);
  expect(yield* query(sql`select incident_id, kind from uptime_alert`)).toEqual([{ incident_id: opened, kind: "opened" }]);
}));

it.live("a removed generated domain deletes the check", () => Effect.gen(function* () {
  const { apply, sync, checks } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  yield* apply({ prefixes: [], healthcheck: none });
  yield* sync;
  expect(yield* checks).toEqual([]);
}));

it.live("a Service leaving Applied State deletes its check, with no sync", () => Effect.gen(function* () {
  const { apply, sync, checks, query } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  // The statement the Store's advance runs for a removed node.
  yield* query(sql`delete from config_applied where environment_id = ${ENVIRONMENT} and node_id = ${SERVICE}`);
  expect(yield* checks).toEqual([]);
}));

it.live("a deleted Environment deletes its checks, with no sync", () => Effect.gen(function* () {
  const { apply, sync, checks, query } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  // The statement the Store's teardown runs.
  yield* query(sql`delete from config_environment where id = ${ENVIRONMENT}`);
  expect(yield* checks).toEqual([]);
}));

// A failed Deployment can still advance Applied State for the nodes it reached before failing.
it.live("a failed Deployment still moves the check to what applied", () => Effect.gen(function* () {
  const { apply, sync, checks, deliver } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: none });
  yield* sync;
  yield* apply({ prefixes: ["web-shop"], healthcheck: http("/up") });
  // No Server is enrolled, so the real runner fails the Deployment.
  const run = yield* deliver(false);
  expect(run.result).toMatchObject({ ran: { id: DEPLOYED, status: "failed" } });
  expect(yield* checks).toEqual([expect.objectContaining({ url: `https://web-shop.${CLUSTER_DOMAIN}/up`, revision: 2 })]);
}));

// The function's concurrency key runs one delivery per Environment at a time, so a retry follows the first.
it.live("a retried delivery converges on one check", () => Effect.gen(function* () {
  const { apply, checks, deliver } = yield* setup();
  yield* apply({ prefixes: ["web-shop"], healthcheck: http("/up") });
  yield* deliver(true);
  const first = yield* checks;
  yield* deliver(true);
  yield* deliver(true);
  expect(first).toHaveLength(1);
  expect(yield* checks).toEqual(first);
}));
