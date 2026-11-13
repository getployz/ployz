import { it } from "@effect/vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect, Fiber } from "effect";
import { expect, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { callStore, readStore } from "#/modules/config-store/config-store.server";
import { bindDoorsRun, executeDoorsOnce, requestDoorsSwitch } from "#/modules/ingress/doors.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORG = "00000000-0000-4000-8000-00000000d401";
const PROJECT = "00000000-0000-4000-8000-00000000d402";
const ENVIRONMENT = "00000000-0000-4000-8000-00000000d403";
const FIRST = "00000000-0000-4000-8000-00000000d404";
const RUN = "run-1";
const production = { project: "shop", environment: "production" };

/** As a runner that applied Deployment `id` records it: each Service node it shipped, as its Saved revision holds it. */
const applied = (id: string) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`update config_deployment set status = 'applied', ended = extract(epoch from now())::bigint where id = ${id}`);
  yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
    select d.environment_id, service->>'id', d.organization_id, d.id, 'service', service::text
    from config_deployment d
    join config_saved s on s.environment_id = d.environment_id and s.revision = d.saved_revision,
    lateral jsonb_array_elements(s.intent::jsonb->'services') service
    where d.id = ${id} and d.nodes::jsonb @> jsonb_build_array(jsonb_build_object('id', service->>'id'))
    on conflict (environment_id, node_id) do update set deployment_id = excluded.deployment_id, node = excluded.node`);
});

/** The Servers, as serving exactly what the Store's Applied State routes, and holding whatever Doors they are told. */
const servers = asTestDouble<PloyzSession>()({
  observeEnrollment: () => Effect.succeed({ network: "10.42.0.0/16", machines: [], target_versions: {} }),
  watchFirstFrame: () => readStore(ORG, { query: "namespace", environment: production }).pipe(
    Effect.map((view) => ({
      machines: [], services: [], unanswered: [], incomplete_ids: { machines: [], containers: [], certificates: [] },
      containers: (view.routes ?? []).map((route) => ({
        kind: "service_container", namespace: view.namespace, resolved_spec: { name: route.service, ports: [{ mode: "ingress", hostname: route.hostname }] },
      })),
    })),
    Effect.orDie,
  ),
  setIngressDoors: (doors: readonly string[]) => Effect.succeed({ machines: [{ server: "s1", doors: [...doors] }], failures: [] }),
});

const write = (command: ConfigCommand) => callStore(ORG, null, { operation: "write", command }).pipe(
  Effect.flatMap((result) => result.ok ? Effect.void : Effect.die(new Error(result.refusal.message))),
);

it.live("a switch redeploys the moving Service as it runs, and leaves every staged edit staged", () => Effect.gen(function* () {
  const services = yield* storeTestCloud();
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(
    Effect.provideService(OrganizationRuntime, { cancel: () => Effect.void, open: () => Effect.succeed({ status: "connected" as const, connected: servers }) }),
    Effect.provide(services),
  );
  yield* provided(Effect.gen(function* () {
    const userId = yield* seedStoreOrganization(ORG);
    yield* enrollStoreServer(ORG);
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at)
      values (${ORG}, 'https://dns.example.test/', 'acme.ployz.test', '{}', now(), now())`);
    yield* drizzle.execute(sql`insert into organization_cloudflare_ingress (organization_id, zone, tunnel_id, tunnel_name, state)
      values (${ORG}, 'example.com', 't1', 'ployz-shop', 'set_up')`);

    yield* write({ command: "create_project", id: PROJECT, name: "shop", default_environment: ENVIRONMENT });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-00000000d405", environment: production, name: "web", image: "nginx:1" });
    yield* write({ command: "create_service", id: "00000000-0000-4000-8000-00000000d406", environment: production, name: "worker", image: "redis:7" });
    yield* write({ command: "add_domain", environment: production, service: "web", hostname: null, prefix: "web", port: 80 });
    yield* write({ command: "add_domain", environment: production, service: "web", hostname: "shop.example.org", port: 80 });
    yield* write({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.env.WORKER", value: "${{ worker.PLOYZ_PRIVATE_DOMAIN }}" }] });
    yield* write({ command: "admit", admit: "deploy", id: FIRST, environment: production, services: [], version: null, accept_volume_loss: [] });
    yield* applied(FIRST);

    yield* write({ command: "remove_domain", environment: production, domain: "shop.example.org" });
    yield* write({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "worker.env.MODE", value: "new" }] });
    const staged = yield* readStore(ORG, { query: "diff", environment: production });
    expect(staged.changes.length).toBeGreaterThan(0);

    const { change } = yield* requestDoorsSwitch({ organizationId: ORG, userId, to: "tunnel" });
    yield* bindDoorsRun({ attemptId: change.attemptId, organizationId: ORG }, RUN);
    const switching = yield* Effect.forkChild(executeDoorsOnce({ attemptId: change.attemptId, organizationId: ORG }, RUN));
    const listed = readStore(ORG, { query: "deployments", environment: production, cursor: null, limit: null });
    const admitted = yield* Effect.promise(() => vi.waitFor(async () => {
      const queued = (await Effect.runPromise(provided(listed))).deployments.find(({ status }) => status === "queued");
      if (queued === undefined) throw new Error("The switch admitted no Deployment yet.");
      return queued;
    }, { timeout: 20_000, interval: 100 }));
    yield* applied(admitted.id);
    expect(yield* Fiber.join(switching)).toMatchObject({ state: "finished" });

    expect({ saved: admitted.saved, ships: admitted.ships }).toEqual({ saved: 1, ships: ["web"] });
    const after = yield* readStore(ORG, { query: "diff", environment: production });
    expect({ saved: after.saved, changes: after.changes }).toEqual({ saved: staged.saved, changes: staged.changes });
    const routes = (yield* readStore(ORG, { query: "namespace", environment: production })).routes?.map(({ hostname }) => hostname).sort();
    expect(routes).toEqual(["shop.example.org", "web.example.com"]);
  }));
}), 60_000);
