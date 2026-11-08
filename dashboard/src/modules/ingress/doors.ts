import type { OwnedNamespace, PublishedHostname } from "@ployz/sdk";
import { Schema } from "effect";

/** Which Door the Cluster ends up behind: its public ports, or the Cloudflare tunnel. */
export const DOORS_WANTS = ["tunnel", "public"] as const;
export type DoorsWant = (typeof DOORS_WANTS)[number];

export const DOORS_STATES = ["pending", "running", "finished", "failed"] as const;
export type DoorsState = (typeof DOORS_STATES)[number];

/**
 * Where a switch is, for the progress the page shows: both Doors open on every Server, generated names moving to the
 * other source, then the old Door closing.
 */
export const DOORS_STAGES = ["opening", "moving", "closing"] as const;
export type DoorsStage = (typeof DOORS_STAGES)[number];

/** A request no run picked up in this long never starts; the hourly sweep closes it. */
export const DOORS_PENDING_LIMIT_MS = 15 * 60_000;
/** The whole switch takes at most this long, Servers answering one at a time. Past it, the change ends failed. */
export const DOORS_RUNNING_LIMIT_MS = 10 * 60_000;

export const SwitchDoorsInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  to: Schema.Literals(DOORS_WANTS),
});
export type SwitchDoorsInput = typeof SwitchDoorsInput.Type;

export const SetUpCloudflareInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  token: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty(), Schema.isMaxLength(512)),
  zone: Schema.String.check(Schema.isNonEmpty(), Schema.isMaxLength(253)),
});
export type SetUpCloudflareInput = typeof SetUpCloudflareInput.Type;

export const ListCloudflareZonesInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  token: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty(), Schema.isMaxLength(512)),
});

/** One switch as the page reads it: each state with exactly its evidence. */
export type LatestDoorsChange =
  | { readonly attemptId: string; readonly want: DoorsWant; readonly state: "pending"; readonly requestedAt: string }
  | { readonly attemptId: string; readonly want: DoorsWant; readonly state: "running"; readonly startedAt: string; readonly stage: DoorsStage | null }
  | { readonly attemptId: string; readonly want: DoorsWant; readonly state: "finished"; readonly endedAt: string }
  | { readonly attemptId: string; readonly want: DoorsWant; readonly state: "failed"; readonly endedAt: string; readonly failure: string };

export type CloudflareSetup = {
  readonly zone: string;
  readonly tunnelName: string;
  readonly tunnelId: string;
  /** `set_up`: the tunnel runs beside the public ports and nothing moved. `left`: switched back, the tunnel is yours to delete. */
  readonly state: "set_up" | "active" | "left";
};

/** What the page reads of the Cluster's ingress without asking a Server: where its names come from and the latest switch. */
export type ClusterIngressView = {
  readonly setup: CloudflareSetup | null;
  readonly change: LatestDoorsChange | null;
  /** The Hosted DNS name generated names sit under on Public. Null before one is reserved. */
  readonly hostedName: string | null;
};

/** A Door by name, as the Engine reports the ones a Server holds open. */
export type DoorName = "public" | "tunnel";

/** What one look at the Servers found: who takes web traffic, which Doors each holds open, and the hostnames published. */
export type ClusterIngressObservation = {
  readonly servers: ReadonlyArray<{ readonly id: string; readonly name: string; readonly doors: readonly DoorName[] }>;
  readonly hostnames: readonly PublishedHostname[];
  /** Some Servers or containers weren't read, so the rest may be incomplete. */
  readonly partial: boolean;
};

/** A generated hostname and where it goes when the switch changes the Cluster Domain's source. */
export type MovedHostname = { readonly from: string; readonly to: string; readonly namespace: string; readonly service: string };

/**
 * Every hostname Services publish under `from` and the one it becomes under `to`. A custom domain isn't under the
 * Cluster Domain, so it doesn't move.
 */
export function movedHostnames(published: readonly PublishedHostname[], from: string, to: string): MovedHostname[] {
  const suffix = `.${from}`;
  return published
    .filter(({ hostname }) => hostname.endsWith(suffix) && !hostname.slice(0, -suffix.length).includes("."))
    .map(({ hostname, namespace, service }) => ({ from: hostname, to: `${hostname.slice(0, -suffix.length)}.${to}`, namespace, service }))
    .sort((a, b) => a.from.localeCompare(b.from));
}

/** An Environment a switch redeploys, with only the Services whose hostnames move, by their names in the Store. */
export type RedeployScope = { readonly project: string; readonly environment: string; readonly services: readonly string[] };

/**
 * What a switch redeploys: each Environment owning a moved hostname, narrowed to the deployed Services publishing one,
 * so nothing that wasn't running starts. `runtimeNames` gives each Namespace's deployed Services by name, as the Store
 * does, since a container carries its Service's runtime name. A hostname whose Service the Store can't name is left
 * over, for the switch to stop on rather than move half the Cluster.
 */
export function redeployScope(
  moving: readonly MovedHostname[], owners: readonly OwnedNamespace[], runtimeNames: ReadonlyMap<string, Readonly<Record<string, string>>>,
) {
  const environments = new Map<string, { project: string; environment: string; services: Set<string> }>();
  const left: MovedHostname[] = [];
  for (const moved of moving) {
    const owner = owners.find(({ namespace }) => namespace === moved.namespace);
    const named = Object.entries(runtimeNames.get(moved.namespace) ?? {}).find(([, runtime]) => runtime === moved.service)?.[0];
    if (owner === undefined || named === undefined) {
      left.push(moved);
      continue;
    }
    const key = `${owner.project}/${owner.environment}`;
    const scope = environments.get(key) ?? { project: owner.project, environment: owner.environment, services: new Set<string>() };
    scope.services.add(named);
    environments.set(key, scope);
  }
  return {
    environments: [...environments.values()].map(({ services, ...environment }) => ({ ...environment, services: [...services].sort() }) satisfies RedeployScope),
    left,
  };
}

/** The words for a switch's direction, as the page and the CLI name it. */
export const wantWords = (want: DoorsWant) => (want === "tunnel" ? "Cloudflare" : "public ports");

/** How the page words a switch under way, from what its row says. */
export function switchProgress(change: LatestDoorsChange | null): { readonly label: string; readonly step: number } | null {
  if (change === null || (change.state !== "pending" && change.state !== "running")) return null;
  const to = wantWords(change.want);
  if (change.state === "pending") return { label: `Starting the switch to ${to}`, step: 0 };
  switch (change.stage) {
    case "moving": return { label: change.want === "tunnel" ? "Moving addresses to your zone" : "Moving addresses back", step: 2 };
    case "closing": return { label: change.want === "tunnel" ? "Closing the public ports" : "Closing the tunnel", step: 3 };
    default: return { label: `Opening ${change.want === "tunnel" ? "the tunnel" : "the public ports"} on every server`, step: 1 };
  }
}

/** How many of `servers` hold `door` open. */
export const holdingCount = (servers: ClusterIngressObservation["servers"], door: DoorName) => servers.filter((server) => server.doors.includes(door)).length;

/** What the Servers seen say of `door`: open on every one, on some, or on none. Null before any Server was seen. */
export function doorWords(observed: ClusterIngressObservation | null, door: DoorName) {
  if (observed === null) return null;
  const servers = observed.servers.length;
  const open = holdingCount(observed.servers, door);
  if (servers === 0) return "No server takes web traffic yet.";
  if (open === servers) return "Open on every server.";
  if (open === 0) return "Closed on every server.";
  return `Open on ${open} of ${servers} servers.`;
}

/** One Server's Doors after a set, by name, as the Engine answers. */
export type DoorsAnswer = {
  readonly machines: ReadonlyArray<{ readonly server: string; readonly doors: readonly string[] }>;
  readonly failures: ReadonlyArray<{ readonly error: { readonly message: string } }>;
};

/**
 * Why a set of Doors didn't land: a Server that refused, or one that answered with other Doors open than asked. Null
 * when every Server holds exactly `wanted`.
 */
export function doorsGap(answer: DoorsAnswer, wanted: readonly string[]) {
  const [first] = answer.failures;
  if (first !== undefined) {
    const count = answer.failures.length;
    return `${count === 1 ? "A server" : `${count} servers`} didn't switch: ${first.error.message}`;
  }
  const off = answer.machines.filter(({ doors }) => doors.length !== wanted.length || !wanted.every((door) => doors.includes(door)));
  if (off.length === 0) return answer.machines.length === 0 ? "No server takes web traffic yet." : null;
  return `${off.map(({ server }) => server).join(", ")} still ${off.length === 1 ? "has" : "have"} other doors open.`;
}

/**
 * Why the Cluster can't switch to `to` now, or null when it can. The source already at `to` refuses, unless the
 * latest switch there failed: its source moved but a step after didn't land, so asking again finishes it.
 */
export function switchProblem(setup: CloudflareSetup | null, to: DoorsWant, latest: LatestDoorsChange | null = null) {
  if (setup === null) return "Set up Cloudflare first, then switch to it.";
  const retrying = latest !== null && latest.state === "failed" && latest.want === to;
  if (to === "tunnel") {
    if (setup.state === "set_up" || (setup.state === "active" && retrying)) return null;
    return setup.state === "active" ? "This Cluster already uses Cloudflare." : "Set up Cloudflare again, then switch to it.";
  }
  return setup.state === "left" && !retrying ? "This Cluster already uses public ports." : null;
}
