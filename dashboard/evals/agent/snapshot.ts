import type { ConfigStore } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import type { JsonValue } from "#/db/tables";
import { Database } from "#/server/database.server";
import { ENVIRONMENTS, type EnvironmentName, environmentRef, ORGANIZATION } from "./fixture";

/** What one Environment holds, as the graders compare it: nothing a Deployment's progress or a clock moves. */
export type EnvironmentState = {
  /** Every Setting by path, defaults included; a secret reads as `{ secret: true }`. */
  readonly settings: Readonly<Record<string, JsonValue>>;
  /** Each domain by `<service> <prefix or hostname>`. */
  readonly domains: Readonly<Record<string, { readonly kind: string; readonly port: number }>>;
  readonly volumes: Readonly<Record<string, { readonly storage: JsonValue; readonly mounts: JsonValue }>>;
  /** Each Deployment's status by number. */
  readonly deployments: Readonly<Record<string, string>>;
  /** The Services of the Saved revision the newest Deployment deploys, by name, each domain as its prefix or hostname. */
  readonly released: Readonly<Record<string, { readonly image: string | null; readonly domains: ReadonlyArray<string> }>>;
};

export type Snapshot = {
  readonly environments: Readonly<Record<EnvironmentName, EnvironmentState>>;
  /** Approvals asked of the Organization so far. */
  readonly approvals: number;
};

type Settings = { settings: ReadonlyArray<{ path: string; value: JsonValue }> };
type Domains = { domains: ReadonlyArray<{ service: string; kind: string; prefix: string | null; hostname: string | null; port: number }> };
type Volumes = { volumes: ReadonlyArray<{ name: string; storage: JsonValue; mounts: JsonValue }> };
type Deployments = { deployments: ReadonlyArray<{ number: number; status: string }> };
type SavedService = {
  slug: string;
  config: { source: { image?: string }; managedHostnames: ReadonlyArray<{ prefix: string }>; routes: ReadonlyArray<{ hostname: string }> };
};

export const snapshot = Effect.fn("Eval.snapshot")(function* (store: ConfigStore) {
  const { drizzle } = yield* Database;
  const environments = yield* Effect.forEach(ENVIRONMENTS, (name) => Effect.gen(function* () {
    const environment = environmentRef(name);
    // SAFETY: each query below names the view it reads, whose shape the type argument spells out.
    const read = <A>(query: Parameters<ConfigStore["read"]>[1]) => Effect.promise(() => store.read(ORGANIZATION, query) as Promise<A>);
    const settings = yield* read<Settings>({ query: "environment", environment, path: null, all: true });
    const domains = yield* read<Domains>({ query: "domains", environment, service: null });
    const volumes = yield* read<Volumes>({ query: "volumes", environment });
    const deployments = yield* read<Deployments>({ query: "deployments", environment, limit: null, cursor: null });
    const [saved] = yield* drizzle.execute<{ intent: string }>(sql`select s.intent from config_deployment d
      join config_environment e on e.id = d.environment_id
      join config_saved s on s.environment_id = d.environment_id and s.revision = d.saved_revision
      where e.name = ${name} order by d.number desc limit 1`, "objects");
    const services: ReadonlyArray<SavedService> = saved === undefined ? [] : JSON.parse(saved.intent).services;
    const state: EnvironmentState = {
      settings: Object.fromEntries(settings.settings.map(({ path, value }) => [path, value])),
      domains: Object.fromEntries(domains.domains.map((domain) =>
        [`${domain.service} ${domain.prefix ?? domain.hostname}`, { kind: domain.kind, port: domain.port }])),
      volumes: Object.fromEntries(volumes.volumes.map(({ name: volume, storage, mounts }) => [volume, { storage, mounts }])),
      deployments: Object.fromEntries(deployments.deployments.map(({ number, status }) => [String(number), status])),
      released: Object.fromEntries(services.map(({ slug, config }) =>
        [slug, {
          image: config.source.image ?? null,
          domains: [...config.managedHostnames.map(({ prefix }) => prefix), ...config.routes.map(({ hostname }) => hostname)],
        }])),
    };
    return [name, state] as const;
  }));
  const [asked] = yield* drizzle.execute<{ count: number }>(
    sql`select count(*)::int as count from operation_approvals where organization_id = ${ORGANIZATION}`, "objects");
  // SAFETY: `environments` holds one entry for each of ENVIRONMENTS.
  return { environments: Object.fromEntries(environments) as Record<EnvironmentName, EnvironmentState>, approvals: asked?.count ?? 0 };
});

/** `snapshot` as one value per key, `staging.settings.web.image` and the like; `released` follows from the rest. */
const flatten = (snapshot: Snapshot) => Object.fromEntries(ENVIRONMENTS.flatMap((name) => {
  const { released: _, ...state } = snapshot.environments[name];
  return Object.entries(state).flatMap(([part, values]) =>
    Object.entries(values).map(([key, value]) => [`${name}.${part}.${key}`, JSON.stringify(value)] as const));
}));

/** Every key whose value differs between `before` and `after`, including keys only one of them has. */
export const changed = (before: Snapshot, after: Snapshot) => {
  const was = flatten(before);
  const is = flatten(after);
  return [...new Set([...Object.keys(was), ...Object.keys(is)])].filter((key) => was[key] !== is[key]).sort();
};
