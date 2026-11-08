import type { PublishedHostname } from "@ployz/sdk";
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

/** What one look at the Servers found: who takes web traffic, who runs a tunnel connector, and the hostnames published. */
export type ClusterIngressObservation = {
  readonly servers: ReadonlyArray<{ readonly id: string; readonly name: string; readonly connected: boolean }>;
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

/** How the page words a switch under way, from what its row says. */
export function switchProgress(change: LatestDoorsChange | null): { readonly label: string; readonly step: number } | null {
  if (change === null || (change.state !== "pending" && change.state !== "running")) return null;
  const to = change.want === "tunnel" ? "Cloudflare" : "public ports";
  if (change.state === "pending") return { label: `Starting the switch to ${to}`, step: 0 };
  switch (change.stage) {
    case "moving": return { label: change.want === "tunnel" ? "Moving addresses to your zone" : "Moving addresses back", step: 2 };
    case "closing": return { label: change.want === "tunnel" ? "Closing the public ports" : "Closing the tunnel", step: 3 };
    default: return { label: `Opening ${change.want === "tunnel" ? "the tunnel" : "the public ports"} on every server`, step: 1 };
  }
}

/** Whether `servers` all run a connector, or the count that does. */
export const connectedCount = (servers: ClusterIngressObservation["servers"]) => servers.filter((server) => server.connected).length;

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

/** Why the Cluster can't switch to `to` now, or null when it can. */
export function switchProblem(setup: CloudflareSetup | null, to: DoorsWant) {
  if (setup === null) return "Set up Cloudflare first, then switch to it.";
  if (to === "tunnel") {
    if (setup.state === "set_up") return null;
    return setup.state === "active" ? "This Cluster already uses Cloudflare." : "Set up Cloudflare again, then switch to it.";
  }
  return setup.state === "left" ? "This Cluster already uses public ports." : null;
}
