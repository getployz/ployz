import type { DeploymentSummary, DeploymentView, DomainRow, ReviewLifecycleKind, ServiceListing } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import type { Marker } from "#/modules/metrics/metrics-model";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
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
 * `since`: when the evidence behind a grey word was current ("Online 2 minutes ago").
 * `checkFailing`: a replica that passed its healthcheck is failing it now, and still serves.
 */
export type RuntimeLine = { word: string; tone: Tone; down: boolean; since: Date | null; checkFailing: boolean };

const line = (word: string, tone: Tone, down = false): RuntimeLine => ({ word, tone, down, since: null, checkFailing: false });

/** No Service is configured to mount the Volume; deployed containers may still use it. */
export const NO_MOUNTS_CONFIGURED = line("No mounts configured", "idle");

/** What evidence says of a Service (`runtime`, null when none names it; `whole`, no Server is missing from it). */
function evidenceLine(runtime: Pick<RuntimeServiceRecord, "containers"> | null, whole: boolean, desiredReplicas: number | null, deploying: boolean) {
  if (!runtime) return whole ? line("Not running", "bad", true) : line("Deployed", "quiet");
  if (runtime.containers.length === 0) return line("Not running", "bad", true);
  const running = runtime.containers.filter((container) => container.runtime?.state === "running");
  if (running.length === 0) return runtime.containers.every((container) => container.runtime?.state === "exited" && container.runtime.code === 0)
    ? line("Stopped", "bad", true) : line("Crashed", "crashed", true);
  const serving = running.filter(containerServing).length;
  if (serving === 0) return line("Starting", "quiet");
  const checkFailing = running.some((container) => container.runtime?.health === "failing");
  // A replica missing from partial evidence may be healthy on the Server that didn't report.
  if (whole && !deploying && desiredReplicas !== null && serving < desiredReplicas) return { ...line("Degraded", "warn"), checkFailing };
  return { ...line("Online", "ok"), checkFailing };
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
 */
export function runtimeLine(
  service: Pick<ServiceListing, "change" | "source">,
  runtime: Pick<RuntimeServiceRecord, "containers"> | null,
  { lens, desiredReplicas, deploying }: { lens: RuntimeLens; desiredReplicas: number | null; deploying: boolean },
): RuntimeLine {
  // A new Service a failed Deploy still started has containers: say what they do.
  if (service.change === "create" && !runtime) return line("Not deployed", "idle");
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
      // The connection dropped: retain the last evidence, or explain why none is available.
      return lens.observedAt === null ? line("Status unavailable", "quiet")
        : { ...evidenceLine(runtime, !lens.incomplete, desiredReplicas, deploying), tone: "quiet", down: false, since: new Date(lens.observedAt), checkFailing: false };
    case "observed":
      return evidenceLine(runtime, !lens.incomplete, desiredReplicas, deploying);
  }
}

/** What a Service's containers went through lately, as its ⚠ says it. An OOM kill outranks a crash loop. */
export type ContainerTrouble = "OOM" | "Restarting";

/** How long a marker counts toward the ⚠ after it happened. */
export const TROUBLE_WINDOW_MS = 60 * 60_000;

/** How long a crash loop stays Restarting after its latest restart. Docker waits at most a minute between restarts. */
export const LOOP_QUIET_MS = 10 * 60_000;

/**
 * What `markers` say at `now` (Unix milliseconds): only those in the hour before it count, however old the read is.
 * A replica's crash loop is its run of restarts that each followed a non-zero exit; a clean exit, as a Deploy or a
 * requested restart leaves, ends the run. The replica is Restarting once its run reaches two, until it stays up for
 * `LOOP_QUIET_MS`.
 */
export function containerTrouble(markers: readonly Pick<Marker, "kind" | "at" | "server" | "replica" | "exitCode">[], now: number): ContainerTrouble | null {
  const recent = markers.filter((marker) => marker.at > now - TROUBLE_WINDOW_MS).sort((a, b) => a.at - b.at);
  if (recent.some((marker) => marker.kind === "oom")) return "OOM";
  const loops = new Map<string, { crashed: boolean; restarts: number; last: number }>();
  for (const marker of recent) {
    const replica = `${marker.server.id}/${marker.replica}`;
    const loop = loops.get(replica) ?? { crashed: false, restarts: 0, last: 0 };
    if (marker.kind === "exit") {
      if (marker.exitCode !== undefined && marker.exitCode !== 0) loops.set(replica, { ...loop, crashed: true });
      else loops.delete(replica);
    } else if (marker.kind === "restart" && loop.crashed) {
      loops.set(replica, { crashed: false, restarts: loop.restarts + 1, last: marker.at });
    }
  }
  return [...loops.values()].some((loop) => loop.restarts >= 2 && loop.last > now - LOOP_QUIET_MS) ? "Restarting" : null;
}

/**
 * A node's ⚠: how many things on it the user can fix, red when one is its Service being down. `trouble` names what its
 * containers went through, and the ⚠ shows it ahead of the count. `monitoring` when an open uptime incident or that
 * trouble is among them, so the ⚠ opens Monitoring, where the evidence is.
 */
export type NodeIssues = { count: number; tone: "bad" | "warn"; monitoring: boolean; trouble: ContainerTrouble | null };

/**
 * What on a node the user can fix: a Service that's down or struggling, a failing healthcheck, domains that need them,
 * the hostname of an open uptime incident (`unreachable`), which is red, and an OOM kill in the last hour or a crash loop
 * (`trouble`), which is amber. Uptime adds nothing when the Service is already down, and shares one issue with a domain
 * needing attention on the same hostname.
 */
export function nodeIssues(status: RuntimeLine, domains: readonly Pick<DomainRow, "status" | "hostname">[],
  { unreachable = null, trouble = null }: { unreachable?: string | null; trouble?: ContainerTrouble | null } = {}): NodeIssues | null {
  const attention = domains.filter((domain) => domain.status === "needs_attention");
  const uptime = !status.down && unreachable !== null;
  const own = uptime && !attention.some((domain) => domain.hostname === unreachable);
  const count = (status.down || status.tone === "warn" ? 1 : 0) + (status.checkFailing ? 1 : 0) + attention.length + (own ? 1 : 0) + (trouble ? 1 : 0);
  return count === 0 ? null : { count, tone: status.down || uptime ? "bad" : "warn", monitoring: own || trouble !== null, trouble };
}

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

/**
 * A Service's chip: anything about Deploys. A Deploy in flight that targets it (running, else queued), else what the
 * next Deploy does to it. `inFlight`: the Environment's Deployments in flight; one naming no Services deploys every one.
 * `restarting`: the Services the running Deploy stops or starts, null until its plan is read. A Service it targets but
 * leaves running (only snapshots its Volume, or nothing changed) is not Deploying.
 */
export function deployChip(
  service: Pick<ServiceListing, "name" | "change">,
  changeCount: number,
  inFlight: readonly Pick<DeploymentSummary, "status" | "services" | "started_at" | "admitted_at">[],
  restarting: ReadonlySet<string> | null = null,
): DeployChipState | null {
  const targeting = inFlight.filter((deployment) => deployment.services.length === 0 || deployment.services.includes(service.name));
  const running = targeting.find((deployment) => deployment.status !== "queued");
  if (running && (restarting === null || restarting.has(service.name))) return { kind: "deploying", since: running.started_at ?? running.admitted_at };
  if (targeting.some((deployment) => deployment.status === "queued")) return { kind: "queued" };
  return service.change === null ? null : stagedChip(service.change, changeCount);
}

export function restartedServices(deployment: Pick<DeploymentView, "nodes">): ReadonlySet<string> {
  return new Set(deployment.nodes.flatMap((node) => node.type === "service" && node.outcome !== "unchanged" ? [node.name] : []));
}

/**
 * A node's staged surface, in the colour of what the next Deploy does to it. None while a Deployment Page is open
 * (`light`, anything but undefined), which lights nodes by its own outcome.
 */
export function stagedSurface(light: Lit | null | undefined, change: ReviewLifecycleKind | null): StagedColour | undefined {
  return light !== undefined || change === null ? undefined : stagedColour(change);
}

/** How many of a Service's secrets have no value, which its Deploys leave out. None when every secret has one. */
export function notSetLabel(notSet: readonly string[]): string | undefined {
  return notSet.length ? `${notSet.length} not set` : undefined;
}
