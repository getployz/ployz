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
/** Cloudflare setup's steps after it reserved the Cluster take at most this long each way: doing, then undoing. */
export const SETUP_RUNNING_LIMIT_MS = 5 * 60_000;
/** A setup reservation older than this belongs to a setup that died; the next setup takes it over. */
export const SETUP_STALE_MS = 2 * SETUP_RUNNING_LIMIT_MS;

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

export const CLOUDFLARE_SETUP_STATES = ["setting_up", "set_up", "active", "left"] as const;
export type CloudflareSetupState = (typeof CLOUDFLARE_SETUP_STATES)[number];

/**
 * The Cluster's Cloudflare setup. `setting_up`: a setup reserved the Cluster and is creating the tunnel; no switch
 * runs until it ends. `set_up`: the tunnel runs beside the public ports and nothing moved. `active`: generated names
 * sit under the zone. `left`: switched back, the tunnel is yours to delete.
 */
export type CloudflareSetup =
  | { readonly zone: string; readonly tunnelName: string; readonly tunnelId: null; readonly state: "setting_up" }
  | { readonly zone: string; readonly tunnelName: string; readonly tunnelId: string; readonly state: "set_up" | "active" | "left" };

/** What the page reads of the Cluster's ingress without asking a Server: where its names come from and the latest switch. */
export type ClusterIngressView = {
  readonly setup: CloudflareSetup | null;
  readonly change: LatestDoorsChange | null;
  /** The Hosted DNS name generated names sit under on Public. Null before one is reserved. */
  readonly hostedName: string | null;
  /** Cloud told the first Server to found the Cluster with every Door closed. What the Servers hold now is observed apart. */
  readonly foundedClosed: boolean;
};

export const CheckCloudflareInput = SetUpCloudflareInput;

/** A Door by name, as the Engine reports the ones a Server holds open. */
export type DoorName = "public" | "tunnel";

/** What one look at the Servers found: who takes web traffic, which Doors each holds open, and the hostnames published. */
export type ClusterIngressObservation = {
  readonly servers: ReadonlyArray<{ readonly id: string; readonly name: string; readonly doors: readonly DoorName[] }>;
  readonly hostnames: readonly PublishedHostname[];
  /** Some Servers or containers weren't read, so the rest may be incomplete. */
  readonly partial: boolean;
};

/**
 * Why a switch can't trust what it saw of the Servers: none answered, or some didn't, so a hostname they publish may be
 * missing from the moves. Null when every Server and container was read.
 */
export function observationGap(observed: ClusterIngressObservation | null) {
  if (observed === null) return "Your servers didn't answer. Try again once they do.";
  if (observed.partial) return "Some servers didn't answer, so an address may be missing. Try again once they do.";
  return null;
}

/** A generated hostname and where it goes when the switch changes the Cluster Domain's source. */
export type MovedHostname = { readonly from: string; readonly to: string; readonly namespace: string; readonly service: string };

/**
 * What produced each hostname the Store's Applied State serves, by {@link routeKey}: a generated domain or a custom
 * one. A published name not here, no Deployment Cloud knows of put there, so a switch can't tell whether it moves.
 */
export type KnownRoutes = ReadonlyMap<string, "generated" | "custom">;
export const routeKey = (namespace: string, hostname: string) => `${namespace}/${hostname}`;

/** The moves whose new hostname no container of its Namespace publishes yet. */
export const unservedMoves = (published: readonly PublishedHostname[], moving: readonly MovedHostname[]) =>
  moving.filter((move) => !published.some(({ hostname, namespace }) => hostname === move.to && namespace === move.namespace));

/**
 * Every hostname Services publish one label under `from` that a Deployment generated there, and the one it becomes
 * under `to`. A custom domain keeps its route whatever the source, so one spelled the same way doesn't move, and
 * only a name `known` as generated moves: Working State can't say, since a domain removed there is still deployed.
 */
export function movedHostnames(published: readonly PublishedHostname[], from: string, to: string, known: KnownRoutes): MovedHostname[] {
  return published
    .flatMap(({ hostname, namespace, service }) => {
      const label = labelUnder(hostname, from);
      return label === null || known.get(routeKey(namespace, hostname)) !== "generated" ? [] : [{ from: hostname, to: `${label}.${to}`, namespace, service }];
    })
    .sort((a, b) => a.from.localeCompare(b.from));
}

/**
 * The moves whose new hostname a custom domain already holds, in any Namespace. The Store judged the two apart under
 * the Cluster Domain it knew, and the switch changes that domain, so both would spell one hostname and the redeploy
 * would be refused on its route IDs.
 */
export function collidingMoves(moving: readonly MovedHostname[], known: KnownRoutes) {
  const custom = new Set([...known].filter(([, kind]) => kind === "custom").map(([key]) => key.slice(key.lastIndexOf("/") + 1)));
  return moving.filter(({ to }) => custom.has(to));
}

/** Why a switch stops on the first of `colliding`: its new hostname is also a custom domain. */
export function collisionGap(colliding: readonly MovedHostname[]) {
  const [first] = colliding;
  if (first === undefined) return null;
  return `${first.to} would be the generated domain of ${first.namespace}/${first.service}, and a custom domain already holds it. Remove the generated domain or the custom domain, deploy, then switch again.`;
}

/** The hostnames published one label under `from` that no Deployment the Store knows of put there. */
export const unknownHostnames = (published: readonly PublishedHostname[], from: string, known: KnownRoutes) =>
  published.filter(({ hostname, namespace }) => labelUnder(hostname, from) !== null && !known.has(routeKey(namespace, hostname)));

/** Why a switch stops on the first of `unknown`: moving it could leave a custom domain behind, and leaving it could strand a generated one. */
export function unknownGap(unknown: readonly PublishedHostname[]) {
  const [first] = unknown;
  if (first === undefined) return null;
  return `${first.hostname} is served by ${first.namespace}/${first.service}, which Ployz Cloud can't match to a deployment. Deploy it again or remove it, then switch again.`;
}

/** The label of `hostname` one level under `zone`, where generated names sit, or null. */
const labelUnder = (hostname: string, zone: string) => {
  const suffix = `.${zone}`;
  if (!hostname.endsWith(suffix)) return null;
  const label = hostname.slice(0, -suffix.length);
  return label.length > 0 && !label.includes(".") ? label : null;
};

/**
 * The custom domains the Servers publish, as the Deployments that put them there say. A switch leaves each where it
 * is, but its DNS still points at the Door the switch closes, so the review names them for the user to repoint.
 */
export const keptHostnames = (published: readonly PublishedHostname[], known: KnownRoutes) =>
  [...new Set(published.filter(({ hostname, namespace }) => known.get(routeKey(namespace, hostname)) === "custom").map(({ hostname }) => hostname))]
    .sort((a, b) => a.localeCompare(b));

/** An Environment a switch redeploys, with only the Services whose hostnames move, by their names in the Store. */
export type RedeployScope = { readonly namespace: string; readonly project: string; readonly environment: string; readonly services: readonly string[] };

/**
 * What a switch redeploys: each Environment owning a moved hostname, narrowed to the deployed Services publishing one,
 * so nothing that wasn't running starts. `runtimeNames` gives each Namespace's deployed Services by name, as the Store
 * does, since a container carries its Service's runtime name. A hostname whose Service the Store can't name is left
 * over, for the switch to stop on rather than move half the Cluster.
 */
export function redeployScope(
  moving: readonly MovedHostname[], owners: readonly OwnedNamespace[], runtimeNames: ReadonlyMap<string, Readonly<Record<string, string>>>,
) {
  const environments = new Map<string, { namespace: string; project: string; environment: string; services: Set<string> }>();
  const left: MovedHostname[] = [];
  for (const moved of moving) {
    const owner = owners.find(({ namespace }) => namespace === moved.namespace);
    const named = Object.entries(runtimeNames.get(moved.namespace) ?? {}).find(([, runtime]) => runtime === moved.service)?.[0];
    if (owner === undefined || named === undefined) {
      left.push(moved);
      continue;
    }
    const key = `${owner.project}/${owner.environment}`;
    const scope = environments.get(key) ?? { namespace: owner.namespace, project: owner.project, environment: owner.environment, services: new Set<string>() };
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

/**
 * What the Servers seen say of `door`: open on every one, on some, or on none. Null before any Server was seen. A look
 * that missed some Servers counts only those seen, and says so, since the rest may hold the Door too.
 */
export function doorWords(observed: ClusterIngressObservation | null, door: DoorName) {
  if (observed === null) return null;
  const servers = observed.servers.length;
  const open = holdingCount(observed.servers, door);
  if (observed.partial) return servers === 0 ? "Some servers didn't answer." : `Open on ${open} of ${servers} servers. Some servers didn't answer.`;
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
  return `${off.map(({ server }) => server).join(", ")} didn't switch.`;
}

/**
 * Why the Cluster can't switch to `to` now, or null when it can. The source already at `to` refuses, unless the
 * latest switch there failed: its source moved but a step after didn't land, so asking again finishes it.
 */
export function switchProblem(setup: CloudflareSetup | null, to: DoorsWant, latest: LatestDoorsChange | null = null) {
  if (setup === null) return "Set up Cloudflare first, then switch to it.";
  if (setup.state === "setting_up") return "Cloudflare is being set up. Wait for it to finish, then switch.";
  const retrying = latest !== null && latest.state === "failed" && latest.want === to;
  if (to === "tunnel") {
    if (setup.state === "set_up" || (setup.state === "active" && retrying)) return null;
    return setup.state === "active" ? "Your servers already use Cloudflare Tunnel." : "Set up Cloudflare again, then switch to it.";
  }
  return setup.state === "left" && !retrying ? "Your servers already use public ports." : null;
}
