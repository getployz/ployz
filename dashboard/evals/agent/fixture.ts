import type { ConfigCommand, ConfigStore, ConfigTrusted, EnvironmentRef } from "@ployz/sdk";
import type { AnyTextAdapter, StreamChunk } from "@tanstack/ai";
import { sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { vi } from "vitest";
import { agentChat } from "#/modules/agent/agent-chat.server";
import { agentPersistence } from "#/modules/agent/persistence.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { decideApproval, pendingApprovals } from "#/modules/approvals/approvals.server";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import type { Caller } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

export const ORGANIZATION = "00000000-0000-4000-8000-0000000e7a01";
const PROJECT = "00000000-0000-4000-8000-0000000e7a02";
export const ENVIRONMENTS = ["production", "staging"] as const;
export type EnvironmentName = (typeof ENVIRONMENTS)[number];
const ENVIRONMENT_IDS = {
  production: "00000000-0000-4000-8000-0000000e7a03",
  staging: "00000000-0000-4000-8000-0000000e7a04",
} satisfies Record<EnvironmentName, string>;
/** Generated prefixes are unique in an Organization, so only staging's web can be `web`. */
const PREFIXES = { production: "www", staging: "web" } satisfies Record<EnvironmentName, string>;
const CLUSTER_DOMAIN = "acme.ployz.test";
/** What made staging's Deployment 4 fail, as its runner recorded it. */
export const FAILURE = "Building worker failed: npm ERR! Missing script: \"start\"";

export const environmentRef = (environment: EnvironmentName): EnvironmentRef => ({ project: "app", environment });

/** The domain evidence Cloud gathers, as a Cluster that publishes nothing would give it. */
const trusted: ConfigTrusted = {
  repositories: [],
  domains: { cluster_domain: { name: CLUSTER_DOMAIN, status: { kind: "ready" } }, certificates: null, ingress_addresses: [], lookups: [] },
  servers: 1,
};

const id = (environment: EnvironmentName, node: number) =>
  `00000000-0000-4000-8000-0000000e${ENVIRONMENTS.indexOf(environment)}${String(node).padStart(3, "0")}`;

/** One Environment's Services as the fixture stages them: web with two variables and a secret, db on pgdata, worker. */
const environmentCommands = (environment: EnvironmentName): ConfigCommand[] => {
  const at = environmentRef(environment);
  const service = (node: number, name: string, image: string): ConfigCommand =>
    ({ command: "create_service", id: id(environment, node), environment: at, name, image });
  return [
    service(10, "web", "nginx:1.27"),
    service(11, "db", "postgres:17"),
    service(12, "worker", "acme/worker"),
    { command: "edit", environment: at, expect: null, changes: [
      { op: "set", path: "web.env.API_URL", value: `https://api.${environment}.acme.test` },
      { op: "set", path: "web.env.LOG_LEVEL", value: "info" },
      { op: "set", path: "web.env.SESSION_KEY", value: { secret: `session-${environment}` } },
    ] },
    { command: "create_volume", id: id(environment, 20), environment: at, name: "pgdata", storage: { kind: "docker" },
      mounts: [{ service: "db", path: "/var/lib/postgresql/data" }] },
    { command: "add_domain", environment: at, service: "web", hostname: null, port: 80 },
    { command: "set_generated_domain", environment: at, service: "web", prefix: PREFIXES[environment] },
  ];
};

/**
 * Organization acme with Project app: production and staging each run web (nginx:1.27, generated domain web), db
 * (postgres:17 on Volume pgdata) and worker (acme/worker). Staging's Deployments 1 to 3 applied its Saved revision and
 * Deployment 4 failed. Inngest only records what Cloud would hand its worker.
 */
export const evalFixture = Effect.fn("Eval.fixture")(function* () {
  const inngest = new Inngest({ id: "agent-eval" });
  const dispatched: unknown[] = [];
  vi.spyOn(inngest, "send").mockImplementation(async (payload) => {
    dispatched.push(payload);
    return { ids: [] };
  });
  const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
  const provided = <A, E, R>(effect: Effect.Effect<A, E, R>) => effect.pipe(Effect.provide(services));
  const userId = yield* provided(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  yield* provided(enrollStoreServer(ORGANIZATION));
  yield* provided(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    const now = new Date();
    yield* drizzle.insert(organizationClusterDomain).values({
      organizationId: ORGANIZATION, endpoint: "http://127.0.0.1:9/", name: CLUSTER_DOMAIN,
      encryptedToken: encryption.encrypt("hosted-dns-token"), reservedAt: now, leaseRenewedAt: now,
    });
  }));
  const store: ConfigStore = yield* provided(cloudStore);
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command, trusted));
  yield* write({ command: "create_project", id: PROJECT, name: "app", default_environment: ENVIRONMENT_IDS.production });
  yield* write({ command: "create_environment", id: ENVIRONMENT_IDS.staging, project: "app", name: "staging" });
  for (const environment of ENVIRONMENTS) yield* Effect.forEach(environmentCommands(environment), write, { discard: true });
  for (const number of [1, 2, 3, 4]) {
    const deployment = id("staging", 30 + number);
    yield* write({ command: "admit", admit: "deploy", id: deployment, environment: environmentRef("staging"), services: [], version: null, accept_volume_loss: [] });
    if (number === 4) yield* Effect.promise(() => store.runDeployment(ORGANIZATION, deployment, "eval-runner", [], { failure: FAILURE }));
    else yield* provided(applied(deployment));
  }
  dispatched.length = 0;
  const caller: Caller = { userId, organization: { id: ORGANIZATION, slug: "acme" }, credential: { kind: "session", id: "session-eval" } };
  /** One sidebar thread of the fixture's member: each turn's stream, and the approval its last turn waits on. */
  const conversation = (model: AnyTextAdapter, thread: string) => {
    let runs = 0;
    const run = (request: Omit<Parameters<typeof agentChat>[1], "threadId" | "runId">) =>
      provided(agentChat(caller, { ...request, threadId: thread, runId: `${thread}-run-${++runs}` }, model)).pipe(Effect.flatMap(collect));
    const say = (content: string) => run({ messages: [{ role: "user", content }] });
    const waiting = provided(Effect.gen(function* () {
      const persistence = yield* agentPersistence({ organizationId: ORGANIZATION, userId });
      const [interrupt] = yield* Effect.promise(() => persistence.stores.interrupts.listPending(thread));
      const [approval] = yield* pendingApprovals(ORGANIZATION);
      return interrupt === undefined || approval === undefined ? null : { interruptId: interrupt.interruptId, approval };
    }));
    /** Answer the waiting approval as a human would in the sidebar, then resume the turn that asked. */
    const answer = (decision: { approve: true } | { deny: string }) => Effect.gen(function* () {
      const asked = (yield* waiting) ?? (yield* Effect.die("nothing is waiting on an approval"));
      yield* provided(decideApproval(caller, asked.approval.id, "approve" in decision
        ? { approve: { digest: asked.approval.digest } }
        : { reject: { reason: decision.deny } }));
      return yield* run({ messages: [], resume: [{ interruptId: asked.interruptId, status: "resolved", payload: {} }] });
    });
    return { say, answer, waiting };
  };
  return { provided, store, write, caller, dispatched, conversation };
});

/** Deployment `deployment` finished: what it deployed is Applied State. */
const applied = Effect.fn(function* (deployment: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`update config_deployment set status = 'applied', ended = 0 where id = ${deployment}`);
  yield* drizzle.execute(sql`insert into config_applied (environment_id, node_id, organization_id, deployment_id, node_type, node)
    select d.environment_id, node->>kind.id, d.organization_id, d.id, kind.type, node::text
    from config_deployment d join config_saved s on s.environment_id = d.environment_id and s.revision = d.saved_revision,
      lateral (values ('service', 'services', 'id'), ('volume', 'volumes', 'resourceId')) kind(type, key, id),
      lateral jsonb_array_elements(coalesce(s.intent::jsonb->kind.key, '[]'::jsonb)) node
    where d.id = ${deployment}
    on conflict (environment_id, node_id) do update set deployment_id = excluded.deployment_id, node = excluded.node`);
});

const collect = (stream: AsyncIterable<StreamChunk>) => Effect.promise(async () => {
  const chunks: StreamChunk[] = [];
  for await (const chunk of stream) chunks.push(chunk);
  return chunks;
});
