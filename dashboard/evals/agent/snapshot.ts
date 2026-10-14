import type { ConfigStore } from "@ployz/sdk";
import { eq, sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import type { JsonValue } from "#/db/tables";
import { operationApprovals } from "#/modules/approvals/tables";
import { machineRemoveAttempt, namespaceCleanup, serverDrainAttempt } from "#/modules/machines/tables";
import { serverUpgradeAttempt } from "#/modules/server-upgrade/tables";
import { volumeRun } from "#/modules/volume-run/tables";
import { Database } from "#/server/database.server";
import { ENVIRONMENTS, type EnvironmentName, environmentRef, ORGANIZATION } from "./fixture";
import { type LedgerEntry, MACHINES_BY_NAME } from "./fleet";

/** What one Environment holds, as the graders compare it: nothing a Deployment's progress or a clock moves. */
export type EnvironmentState = {
  /** Every Setting by path, defaults included; a secret reads as `{ secret: true }`. */
  readonly settings: Readonly<Record<string, JsonValue>>;
  /** Each domain by `<service> <prefix or hostname>`. */
  readonly domains: Readonly<Record<string, { readonly kind: string; readonly port: number | null }>>;
  readonly volumes: Readonly<Record<string, { readonly storage: JsonValue; readonly mounts: JsonValue }>>;
  /** Each Deployment's status by number. */
  readonly deployments: Readonly<Record<string, string>>;
  /** The Services of the Saved revision the newest Deployment deploys, by name, each domain as its prefix or hostname. */
  readonly released: Readonly<Record<string, { readonly image: string | null; readonly domains: ReadonlyArray<string> }>>;
};

/** How many times each key happened. Two snapshots compare per key, so a duplicate shows even when the end state matches. */
export type Multiset = Readonly<Record<string, number>>;

export type Snapshot = {
  readonly environments: Readonly<Record<EnvironmentName, EnvironmentState>>;
  /** Each Project's Environments by name. */
  readonly projects: Readonly<Record<string, ReadonlyArray<string>>>;
  /** Approvals asked of the Organization so far. */
  readonly approvals: number;
  /**
   * What Cloud started: `event <name> <id>` for each Inngest event, `attempt drain|upgrade|remove <server>`,
   * `attempt cleanup <namespace>`, `volume-run <kind> <volume>` and `approval <command>`.
   */
  readonly actions: Multiset;
  /** Each request that changes a Machine, as `<server> <command> <subject>`, from the fake fleet's ledger. */
  readonly machine: Multiset;
};

/** What the snapshot reads besides the database: the events Cloud sent and the fake fleet's ledger. */
export type Sources = { readonly dispatched: ReadonlyArray<unknown>; readonly ledger: ReadonlyArray<LedgerEntry> };

const count = (keys: ReadonlyArray<string>) => {
  const counts = new Map<string, number>();
  for (const key of keys) counts.set(key, (counts.get(key) ?? 0) + 1);
  return Object.fromEntries(counts) satisfies Multiset;
};

/** Requests that only read a Machine, which the grader ignores. */
const READ = /^(inspect|list|describe|get)_|^(machine_token|log_history|tail_logs|runtime_watch)$/;
const NAMES = new Map<string, string>(Object.values(MACHINES_BY_NAME).map(({ id, name }) => [id, name]));

/** What a request acts on: its container, its upgrade attempt or its Volume, else nothing. */
const subject = (entry: LedgerEntry) => entry.container ?? entry.request?.attempt_id ?? entry.request?.name ?? "";

/** `ledger`'s Machine effects as a multiset. */
export const effects = (ledger: ReadonlyArray<LedgerEntry>): Multiset =>
  count(ledger.filter(({ command }) => !READ.test(command)).map((entry) => `${NAMES.get(entry.machine) ?? entry.machine} ${entry.command} ${subject(entry)}`.trim()));

const decodeEvent = Schema.decodeUnknownOption(Schema.Struct({ name: Schema.String, id: Schema.optionalKey(Schema.String) }));

/** Each Inngest event in what Cloud handed `send`, one payload or an array of them, by name and id. */
const events = (dispatched: ReadonlyArray<unknown>) => dispatched.flat().flatMap((payload) =>
  Option.match(decodeEvent(payload), { onNone: () => [], onSome: ({ name, id }) => [`event ${name} ${id ?? "-"}`] }));

type SavedService = {
  slug: string;
  config: { source: { image?: string }; managedHostnames: ReadonlyArray<{ prefix: string }>; routes: ReadonlyArray<{ hostname: string }> };
};

export const snapshot = Effect.fn("Eval.snapshot")(function* (store: ConfigStore, sources: Sources) {
  const { drizzle } = yield* Database;
  const environments = yield* Effect.forEach(ENVIRONMENTS, (name) => Effect.gen(function* () {
    const environment = environmentRef(name);
    const settings = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "environment", environment, path: null, all: true }));
    const domains = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "domains", environment, service: null }));
    const volumes = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "volumes", environment }));
    const deployments = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "deployments", environment, limit: null, cursor: null }));
    const [saved] = yield* drizzle.execute<{ intent: string }>(sql`select s.intent from config_deployment d
      join config_environment e on e.id = d.environment_id
      join config_saved s on s.environment_id = d.environment_id and s.revision = d.saved_revision
      where e.name = ${name} order by d.number desc limit 1`, "objects");
    const services: ReadonlyArray<SavedService> = saved === undefined ? [] : JSON.parse(saved.intent).services;
    const state: EnvironmentState = {
      settings: Object.fromEntries(settings.settings.map(({ path, value }) => [path, value])),
      domains: Object.fromEntries(domains.domains.map((domain) =>
        [`${domain.service} ${domain.kind === "generated" ? domain.prefix : domain.hostname}`, { kind: domain.kind, port: domain.port }])),
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
  const { projects } = yield* Effect.promise(() => store.read(ORGANIZATION, { query: "projects" }));
  const mine = eq(operationApprovals.organizationId, ORGANIZATION);
  const approvals = yield* drizzle.select({ command: operationApprovals.command }).from(operationApprovals).where(mine);
  const name = (machine: string) => NAMES.get(machine) ?? machine;
  const drains = yield* drizzle.select({ machine: serverDrainAttempt.machineId }).from(serverDrainAttempt).where(eq(serverDrainAttempt.organizationId, ORGANIZATION));
  const upgrades = yield* drizzle.select({ machine: serverUpgradeAttempt.machineId, attempt: serverUpgradeAttempt.attemptId })
    .from(serverUpgradeAttempt).where(eq(serverUpgradeAttempt.organizationId, ORGANIZATION));
  const removals = yield* drizzle.select({ machine: machineRemoveAttempt.machineId }).from(machineRemoveAttempt).where(eq(machineRemoveAttempt.organizationId, ORGANIZATION));
  const cleanups = yield* drizzle.select({ namespace: namespaceCleanup.namespace }).from(namespaceCleanup).where(eq(namespaceCleanup.organizationId, ORGANIZATION));
  const runs = yield* drizzle.select({ kind: volumeRun.kind, volume: volumeRun.volumeName }).from(volumeRun).where(eq(volumeRun.organizationId, ORGANIZATION));
  return {
    // SAFETY: `environments` holds one entry for each of ENVIRONMENTS.
    environments: Object.fromEntries(environments) as Record<EnvironmentName, EnvironmentState>,
    projects: Object.fromEntries(projects.map(({ name: project, environments: names }) => [project, [...names].sort()])),
    approvals: approvals.length,
    actions: count([
      ...events(sources.dispatched),
      ...drains.map(({ machine }) => `attempt drain ${name(machine)}`),
      ...upgrades.map(({ machine, attempt }) => `attempt upgrade ${name(machine)} ${attempt}`),
      ...removals.map(({ machine }) => `attempt remove ${name(machine)}`),
      ...cleanups.map(({ namespace }) => `attempt cleanup ${namespace}`),
      ...runs.map(({ kind, volume }) => `volume-run ${kind} ${volume}`),
      ...approvals.map(({ command }) => `approval ${command}`),
    ]),
    machine: effects(sources.ledger),
  } satisfies Snapshot;
});

/** How many more times each key of `after` happened than in `before`, keys that did not change left out. */
export const delta = (before: Multiset, after: Multiset): Multiset => Object.fromEntries(
  Object.entries(after).flatMap(([key, times]) => (times === (before[key] ?? 0) ? [] : [[key, times - (before[key] ?? 0)] as const])));

/**
 * `snapshot` as one value per key, `staging.settings.web.image`, `projects.trial` and `machine.web-1 ensure_container:stopped …`;
 * `released` follows from the rest, and `actions` the tasks grade themselves.
 */
const flatten = (snapshot: Snapshot) => Object.fromEntries([
  ...ENVIRONMENTS.flatMap((name) => {
    const { released: _, ...state } = snapshot.environments[name];
    return Object.entries(state).flatMap(([part, values]) =>
      Object.entries(values).map(([key, value]) => [`${name}.${part}.${key}`, JSON.stringify(value)] as const));
  }),
  ...Object.entries(snapshot.projects).map(([project, names]) => [`projects.${project}`, JSON.stringify(names)] as const),
  ...Object.entries(snapshot.machine).map(([key, times]) => [`machine.${key}`, String(times)] as const),
]);

/** Every key whose value differs between `before` and `after`, including keys only one of them has. */
export const changed = (before: Snapshot, after: Snapshot) => {
  const was = flatten(before);
  const is = flatten(after);
  return [...new Set([...Object.keys(was), ...Object.keys(is)])].filter((key) => was[key] !== is[key]).sort();
};
