import type { DeploymentSummary, DomainRow, ReviewLifecycleKind, ServiceListing, VolumeListing } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import type { RuntimeServiceRecord, RuntimeVolumeRecord } from "#/modules/runtime/runtime.collection";
import type { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { containerServing } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";
import type { Lit } from "../deployment-page";

/**
 * How a status line reads: its colour; `crashed`, red with the explosion; `idle`, a hollow dot for a Service with
 * nothing to run yet; `quiet`, grey, for evidence that isn't current or a Service still starting; `pending`, a shimmer
 * while the first evidence comes in; `unreachable`, a cloud-off icon.
 */
export type Tone = "ok" | "warn" | "bad" | "crashed" | "quiet" | "idle" | "pending" | "unreachable";

/**
 * What runs now, in one word. `down`: it should serve and doesn't, which turns the card's border red.
 * `since`: when the evidence behind a grey word was current ("Online 2 minutes ago"), or when it crashed.
 * `code`: a crash's exit code, which only its panel shows.
 */
export type RuntimeLine = { word: string; tone: Tone; down: boolean; since: Date | null; code: number | null };

const line = (word: string, tone: Tone, down = false): RuntimeLine => ({ word, tone, down, since: null, code: null });

/** A Volume's status line while nothing here mounts it. */
export const NOT_MOUNTED = line("Not mounted", "idle");

/** "Crashed" since the newest stop among its containers, "Out of memory" when that one was OOM-killed. */
function crashedLine(containers: RuntimeServiceRecord["containers"]): RuntimeLine {
  const stops = containers.flatMap(({ runtime }) => runtime?.state === "exited" ? [runtime] : []);
  // RFC 3339 in UTC at one precision sorts as text; a stop with no time sorts last.
  const last = stops.flatMap((stop) => stop.stopped_at === null ? [] : [{ ...stop, stopped_at: stop.stopped_at }])
    .sort((a, b) => b.stopped_at.localeCompare(a.stopped_at))[0];
  if (!last) return { ...line("Crashed", "crashed", true), code: stops[0]?.code ?? null };
  return { ...line(last.oom_killed ? "Out of memory" : "Crashed", "crashed", true), since: new Date(last.stopped_at), code: last.code };
}

/**
 * What evidence says of a Service (`runtime`, null when none names it; `whole`, no Server is missing from it).
 * `chip`: a Deploy in flight that targets it; until its first container runs, it's Starting.
 */
function evidenceLine(runtime: Pick<RuntimeServiceRecord, "containers"> | null, whole: boolean, desiredReplicas: number | null, chip: DeployChipState | null) {
  const inFlight = chip?.kind === "deploying" || chip?.kind === "queued";
  if (inFlight && !runtime?.containers.length) return line("Starting", "quiet");
  if (!runtime) return whole ? line("Not running", "bad", true) : line("Deployed", "quiet");
  if (runtime.containers.length === 0) return line("Not running", "bad", true);
  const running = runtime.containers.filter((container) => container.runtime?.state === "running");
  // With a Server missing from the evidence, a replica may run there: claim no crash.
  if (running.length === 0) return whole ? crashedLine(runtime.containers) : line("Not seen running", "quiet");
  const serving = running.filter(containerServing).length;
  // None serves yet: Unhealthy once a health check fails, else still Starting.
  if (serving === 0) return runtime.containers.some(({ runtime }) => runtime?.state === "running" && runtime.health === "unhealthy") ? line("Unhealthy", "warn") : line("Starting", "quiet");
  // A replica missing from partial evidence may be healthy on the Server that didn't report.
  if (whole && chip?.kind !== "deploying" && desiredReplicas !== null && serving < desiredReplicas) return line("Degraded", "warn");
  return line("Online", "ok");
}

/**
 * What the Runtime Watch says of the Servers, as `useRuntimeLens` reads it: its status, whether a Server is missing from
 * its evidence, when that was current, and whether there is no Server at all.
 */
export type RuntimeLens = Pick<ReturnType<typeof useRuntimeLens>, "status" | "incomplete" | "observedAt" | "noServers">;

/**
 * A Service's status line: what runs now, from runtime evidence. Staged work and Deploys never replace it, and it never
 * guesses: before evidence it waits, and evidence that isn't current reads grey with its age.
 * `desiredReplicas`: how many it asks for, when known; fewer serving reads Degraded, except while a Deploy rolls them.
 * `chip`: its Deploy chip, which says whether a Deploy in flight targets it.
 */
export function runtimeLine(
  service: Pick<ServiceListing, "change" | "source">,
  runtime: Pick<RuntimeServiceRecord, "containers"> | null,
  { lens, desiredReplicas, chip }: { lens: RuntimeLens; desiredReplicas: number | null; chip: DeployChipState | null },
): RuntimeLine {
  if (service.change === "create") return line("Not deployed", "idle");
  if (service.source === "empty") return line("No source", "idle");
  if (lens.noServers) return line("Needs a server", "idle");
  switch (lens.status) {
    case "no_connection":
      return line("Needs a server", "idle");
    case "unreachable":
      return line("Can't reach servers", "unreachable");
    case "connecting":
      return line("Checking", "pending");
    case "unavailable":
      // The connection dropped: the last evidence, grey, from when it was current; none seen yet, it waits.
      return lens.observedAt === null ? line("Checking", "pending")
        : { ...evidenceLine(runtime, !lens.incomplete, desiredReplicas, chip), tone: "quiet", down: false, since: new Date(lens.observedAt) };
    case "observed":
      return evidenceLine(runtime, !lens.incomplete, desiredReplicas, chip);
  }
}

/** One thing on a node the user can fix: its Service down or struggling, a domain that needs them, a Volume filling up. */
export type NodeIssue =
  | { kind: "runtime"; line: RuntimeLine }
  | { kind: "domain"; domain: DomainRow }
  | { kind: "volume"; volume: Pick<VolumeListing, "id" | "name" | "storage_locked">; fill: number; tone: "warn" | "bad" };

/**
 * What on a node the user can fix, one per line of its panel's list; its ⚠ N counts them. `volumes`: each Volume with its
 * `volumeFill`.
 */
export function nodeIssues(
  status: RuntimeLine,
  domains: readonly DomainRow[],
  volumes: readonly { volume: Pick<VolumeListing, "id" | "name" | "storage_locked">; fill: number | null }[],
): NodeIssue[] {
  return [
    ...(status.down || status.tone === "warn" ? [{ kind: "runtime", line: status } as const] : []),
    ...domains.flatMap((domain) => domain.status === "needs_attention" ? [{ kind: "domain", domain } as const] : []),
    ...volumes.flatMap(({ volume, fill }) => {
      const tone = fillTone(fill);
      return tone === null || fill === null ? [] : [{ kind: "volume", volume, fill, tone } as const];
    }),
  ];
}

/** The Docker Volume holding a Volume's data on each Server, as ployz-store's `docker_volume` names it: `vol-{id}` in the Namespace. */
export const dockerVolumeName = (namespace: string, volumeId: string) => `${namespace}_vol-${volumeId}`;

/**
 * How full a Volume is, 0 to 1, on the fullest Server holding it (one per Server); null when none reports a bound, as
 * plain Docker storage never does.
 */
export function volumeFill(volumes: readonly Pick<RuntimeVolumeRecord, "name" | "usedBytes" | "boundBytes">[], dockerVolume: string): number | null {
  const fills = volumes.filter((volume) => volume.name === dockerVolume && volume.boundBytes > 0).map((volume) => volume.usedBytes / volume.boundBytes);
  return fills.length === 0 ? null : Math.min(1, Math.max(...fills));
}

/** A fill that needs the user: amber from 80%, red from 95%; null below. */
export function fillTone(fill: number | null): "warn" | "bad" | null {
  if (fill === null || fill < 0.8) return null;
  return fill < 0.95 ? "warn" : "bad";
}

/** A fill as copy: "92% full". Rounded down, so 99.6% never reads full. */
export const fillText = (fill: number) => `${Math.floor(fill * 100)}% full`;

/** The public domain a Service's card shows, of its domains: the first custom one, else the generated one; `live` once it's set up. */
export function publicDomain(domains: readonly DomainRow[]) {
  const shown = domains.find((domain) => domain.kind === "custom") ?? domains.find((domain) => domain.kind === "generated" && domain.hostname !== null);
  return shown?.hostname ? { hostname: shown.hostname, live: shown.status !== "setting_up" } : null;
}

/** What the next Deploy does to a node, in colour: green when it creates it, blue when it changes it, red when it removes it. */
export type StagedColour = "success" | "info" | "destructive";

const stagedColour = (change: ReviewLifecycleKind): StagedColour =>
  change === "create" ? "success" : change === "delete" ? "destructive" : "info";

/**
 * A node's chip: a Deploy in flight that targets it, running since `since` (Unix seconds) or queued, else what the next
 * Deploy does to it.
 */
export type DeployChipState =
  | { kind: "deploying"; since: number }
  | { kind: "queued" }
  | { kind: "staged"; label: string; variant: StagedColour };

/** What the next Deploy does to a node, as its chip says it. */
export function stagedChip(change: ReviewLifecycleKind, changeCount: number): DeployChipState {
  const variant = stagedColour(change);
  if (change === "create") return { kind: "staged", label: "New", variant };
  if (change === "delete") return { kind: "staged", label: "Removing", variant };
  return { kind: "staged", label: changeCount === 0 ? "Changed" : plural(changeCount, "change"), variant };
}

/** A Deployment in flight with the ids of the nodes it targets (`nodes`), null until its view arrives. */
export type InFlightTargets = Pick<DeploymentSummary, "status" | "started_at" | "admitted_at"> & { nodes: readonly string[] | null };

/**
 * A Service's chip: anything about Deploys. A Deploy in flight that targets it (running, else queued), else what the
 * next Deploy does to it. `inFlight`: the Environment's Deployments in flight; one targets nothing until its nodes arrive.
 */
export function deployChip(
  service: Pick<ServiceListing, "id" | "change">,
  changeCount: number,
  inFlight: readonly InFlightTargets[],
): DeployChipState | null {
  const targeting = inFlight.filter((deployment) => deployment.nodes?.includes(service.id));
  const running = targeting.find((deployment) => deployment.status !== "queued");
  if (running) return { kind: "deploying", since: running.started_at ?? running.admitted_at };
  if (targeting.length > 0) return { kind: "queued" };
  return service.change === null ? null : stagedChip(service.change, changeCount);
}

/**
 * A node's staged surface, in the colour of what the next Deploy does to it. None while a Deployment Page is open
 * (`light`, anything but undefined), which lights nodes by its own outcome.
 */
export function stagedSurface(light: Lit | null | undefined, change: ReviewLifecycleKind | null): StagedColour | undefined {
  return light !== undefined || change === null ? undefined : stagedColour(change);
}
