import type { RuntimeWatchView } from "@ployz/sdk";
import { Schema } from "effect";
import { closedHolderAnywhere, publicSpecifiedAnywhere } from "#/modules/cluster-domain/ingress-running";
import { offlineServers } from "#/modules/machines/server-status";
import { type Requirement, requirements } from "#/modules/runtime/daemon-compat";

/** Capability names as a ployz lists them to Cloud before it joins. Absent from a ployz older than any list, which claims nothing. */
export const AdvertisedCapabilities = Schema.Array(Schema.String.check(Schema.isMaxLength(128))).check(Schema.isMaxLength(512));

/** Something a Cluster has turned on that every Server in it must carry out. */
export type ClusterFeature = "closedDoors";

type ClusterRequirement = {
  readonly requirement: Requirement;
  /** What the Cluster does, as a sentence a joining Server's operator reads. */
  readonly cluster: string;
  /** The feature in a phrase that follows "a Cluster with". */
  readonly phrase: string;
  /** What a first Server lacking it is told. */
  readonly founder: string;
};

/**
 * The one list of Cluster Requirements: each feature a Cluster can turn on, and the capability a Server needs to be in
 * it. A Server whose ployz lists no capabilities lacks every one, so it is refused only by a Cluster that has turned a
 * feature on. A new row refuses joiners and names Servers that lack its capability, with no other edit.
 */
export const CLUSTER_REQUIREMENTS = {
  closedDoors: {
    requirement: requirements.closedDoors,
    cluster: "This Cluster keeps ports 80 and 443 closed.",
    phrase: "closed ports 80 and 443",
    founder: "Closed ports 80 and 443 need a newer ployz on the first server. Update ployz on it, or get an install command without Cloudflare Tunnel.",
  },
} as const satisfies Record<ClusterFeature, ClusterRequirement>;

// SAFETY: the table satisfies Record<ClusterFeature, ClusterRequirement>, so its keys are exactly the ClusterFeature names.
const FEATURES = Object.keys(CLUSTER_REQUIREMENTS) as readonly ClusterFeature[];

type Frame = Pick<RuntimeWatchView, "services" | "machines" | "incomplete_ids" | "unanswered">;

/** The features of a Cluster a first Server is about to found, read from the Fleet Token that asks for it. */
export function foundingFeatures(founder: { readonly foundsClosed?: boolean }): ReadonlySet<ClusterFeature> {
  return new Set(founder.foundsClosed === true ? ["closedDoors"] : []);
}

/**
 * Whether a Server didn't answer, or a row wasn't read: what a Server shows then is as it last reported, so what the
 * Cluster holds now is unknown.
 */
export function partialFrame(frame: Pick<RuntimeWatchView, "machines" | "incomplete_ids" | "unanswered">): boolean {
  const offline = offlineServers(frame.machines.map(({ machine, membership }) =>
    ({ id: machine.id, membership, runningBuilds: machine.runtime.running_builds, unanswered: frame.unanswered.includes(machine.id) })));
  return frame.incomplete_ids.machines.length > 0 || frame.incomplete_ids.containers.length > 0 || offline.size > 0;
}

/**
 * The features a Cluster has turned on. Seen whole, the Servers are the truth: a holder that publishes no port keeps the
 * Doors closed, and holders that all publish 80 and 443 mean they are open whatever Cloud once asked. A Cluster seen
 * whole with no holder yet, partly, or not at all keeps what Cloud recorded asking for, since a Server not seen may hold
 * the closed holder and a founder's holder may not show yet.
 */
export function featuresOf(evidence: { readonly recorded: ReadonlySet<ClusterFeature>; readonly frame: Frame | null }): ReadonlySet<ClusterFeature> {
  const { recorded, frame } = evidence;
  if (frame === null) return recorded;
  if (closedHolderAnywhere(frame)) return new Set<ClusterFeature>([...recorded, "closedDoors"]);
  if (!partialFrame(frame) && publicSpecifiedAnywhere(frame)) return new Set();
  return recorded;
}

/**
 * The check every join path asks. The first feature of `features` whose capability `advertised` doesn't list. An absent
 * or empty list lacks every capability, so a ployz older than the list is refused only where the Cluster requires something.
 */
export function missingFor(features: ReadonlySet<ClusterFeature>, advertised: readonly string[] | undefined): ClusterFeature | null {
  return FEATURES.find((feature) => features.has(feature) && advertised?.includes(CLUSTER_REQUIREMENTS[feature].requirement.capability) !== true) ?? null;
}

/** Whether `advertised` lists every Cluster Requirement, so a joiner needs no evidence read. */
export function listsEvery(advertised: readonly string[] | undefined): boolean {
  return missingFor(new Set(FEATURES), advertised) === null;
}

type Listed = { readonly name: string; readonly accepts_ingress: boolean; readonly runtime: { readonly capabilities?: readonly string[] } };

/** The Servers in `frame` that lack a capability `features` require. A Server whose record lists none lacks it. */
export function serversMissing(
  frame: { readonly machines: ReadonlyArray<{ readonly machine: Listed }> },
  features: ReadonlySet<ClusterFeature>,
  only: (machine: Listed) => boolean = () => true,
): string[] {
  return frame.machines.filter(({ machine }) => only(machine) && missingFor(features, machine.runtime.capabilities) !== null).map(({ machine }) => machine.name);
}

/** A joiner's refusal, read by its CLI. */
export function joinWords(feature: ClusterFeature): string {
  return `${CLUSTER_REQUIREMENTS[feature].cluster} Update ployz on this server to join it.`;
}

/** A host's reason in the enroll list. */
export function skipWords(feature: ClusterFeature): string {
  return `Update ployz to join a Cluster with ${CLUSTER_REQUIREMENTS[feature].phrase}`;
}

/** A first Server's refusal. */
export function founderWords(feature: ClusterFeature): string {
  return CLUSTER_REQUIREMENTS[feature].founder;
}

/** A feature that can't turn on while Servers already in the Cluster lack it. */
export function lackingWords(servers: readonly string[], goal: string): string {
  const named = servers.length <= 1 ? servers.join("") : `${servers.slice(0, -1).join(", ")} and ${servers.at(-1)}`;
  return `Update ployz on ${named} to ${goal}.`;
}
