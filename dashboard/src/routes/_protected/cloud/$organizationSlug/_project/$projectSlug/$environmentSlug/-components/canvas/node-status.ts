import type { DeploymentSummary, DeploymentView, DomainRow, ReviewLifecycleKind, ServiceListing } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import { formatRelativeTime } from "#/utils/relative-time";
import { shippedService } from "#/modules/config-store/store-deployments";
import type { CachedStoreRead } from "#/modules/config-store/store-view.queries";
import type { Marker } from "#/modules/metrics/metrics-model";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import type { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { containerServing } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";

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

/**
 * What the running Deploy does to a Service, if any runs: `restarts` it; `leaves` it as it is, as when none runs;
 * `undecided` while the dashboard can't tell yet (its view unread, or no plan recorded and no new image built for it);
 * `unreadable` when its view was refused or its read failed, so nothing will tell.
 */
export type DeployEffect = "restarts" | "leaves" | "undecided" | "unreadable";

const NO_CONTAINER = {
  restarts: line("Starting", "quiet"),
  undecided: line("Checking", "pending"),
  unreadable: line("Status unavailable", "quiet"),
  leaves: line("Not running", "bad", true),
} satisfies Record<DeployEffect, RuntimeLine>;

/**
 * What evidence says of a Service (`runtime`, null when none names it; `whole`, no Server is missing from it;
 * `offline`, the Servers whose containers are only as they last reported them).
 */
function evidenceLine(runtime: Pick<RuntimeServiceRecord, "containers"> | null, whole: boolean, offline: ReadonlySet<string>, desiredReplicas: number | null, rolling: boolean, noContainer: RuntimeLine) {
  if ((!runtime || runtime.containers.length === 0) && !whole) return line("Deployed", "quiet");
  if (!runtime || runtime.containers.length === 0) return noContainer;
  const containers = runtime.containers.filter((container) => !offline.has(container.machineId));
  // A Server missing from partial evidence may still serve it.
  if (containers.length === 0) return whole ? line("Server offline", "bad", true) : line("Server offline", "quiet");
  const running = containers.filter((container) => container.runtime?.state === "running");
  if (running.length === 0 && !whole) return line("Not seen running", "quiet");
  // A run its Server stopped on request is no crash, whatever code the app exited with.
  if (running.length === 0) return containers.every((container) => container.runtime?.state === "exited" && (container.runtime.code === 0 || container.runtime.stopped === true))
    ? line("Stopped", "bad", true) : line("Crashed", "crashed", true);
  const serving = running.filter(containerServing).length;
  if (serving === 0) return line("Starting", "quiet");
  const checkFailing = running.some((container) => container.runtime?.health === "failing");
  // A replica missing from partial evidence may be healthy on the Server that didn't report.
  if (whole && !rolling && desiredReplicas !== null && serving < desiredReplicas) return { ...line("Degraded", "warn"), checkFailing };
  return { ...line("Online", "ok"), checkFailing };
}

/**
 * What the Runtime Watch says of the Servers, as `useRuntimeLens` reads it: its status, whether a Server is missing from
 * its evidence, when that was current, whether there is no Server at all, and which Servers are offline.
 */
export type RuntimeLens = Pick<ReturnType<typeof useRuntimeLens>, "status" | "incomplete" | "observedAt" | "noServers" | "offline">;

/**
 * A Service's status line: what runs now, from runtime evidence. Staged work never replaces it, and it never guesses:
 * before evidence it waits, and evidence that isn't current reads grey with its age.
 * `deploy`: what the running Deploy does to it, which picks what current, whole evidence naming no container of it
 * reads (`NO_CONTAINER`); evidence that isn't current keeps its last word;
 * any container it has still says what it says: a Service already down stays down.
 * `desiredReplicas`: how many it asks for, when known; fewer serving reads Degraded, except while a Deploy rolls them.
 */
export function runtimeLine(
  service: Pick<ServiceListing, "change" | "source">,
  runtime: Pick<RuntimeServiceRecord, "containers"> | null,
  { lens, desiredReplicas, deploy }: { lens: RuntimeLens; desiredReplicas: number | null; deploy: DeployEffect },
): RuntimeLine {
  const rolling = deploy === "restarts";
  // A new Service a failed Deploy still started has containers, and one a Deploy is starting has none yet: say what they do.
  if (service.change === "create" && !runtime && !rolling) return line("Not deployed", "idle");
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
        : { ...evidenceLine(runtime, !lens.incomplete, lens.offline, desiredReplicas, rolling, NO_CONTAINER.leaves), tone: "quiet", down: false, since: new Date(lens.observedAt), checkFailing: false };
    case "observed":
      // A Deploy that removes it stops it for good: nothing is starting.
      return evidenceLine(runtime, !lens.incomplete, lens.offline, desiredReplicas, rolling, NO_CONTAINER[service.change === "delete" ? "leaves" : deploy]);
  }
}

/** What a Service's containers went through lately, as its ⚠ says it. An OOM kill outranks a crash loop. */
export type ContainerTrouble = "OOM" | "Restarting";

/** How long a marker counts toward the ⚠ after it happened. */
export const TROUBLE_WINDOW_MS = 60 * 60_000;

/** How long a crash loop stays Restarting after its latest restart. Docker waits at most a minute between restarts. */
export const LOOP_QUIET_MS = 10 * 60_000;

/**
 * Exit codes (128 + signal) of the signals a stop sends rather than a fault raises: SIGHUP, SIGINT, SIGQUIT, SIGKILL,
 * SIGUSR1, SIGUSR2 and SIGTERM. Docker never restarts a stopped container and an OOM kill has its own marker, so a
 * restart after one of these was asked for. Faults such as SIGSEGV or SIGABRT stay crashes.
 */
const STOP_SIGNAL_EXITS: ReadonlySet<number> = new Set([129, 130, 131, 137, 138, 140, 143]);

/**
 * What `markers` say at `now` (Unix milliseconds): only those in the hour before it count, however old the read is.
 * A replica's crash loop is its run of restarts that each followed a crash: a non-zero exit not from a signal a stop sends. Any
 * other exit, as a Deploy or a requested restart leaves, ends the run, and so does staying up for `LOOP_QUIET_MS`. The
 * replica is Restarting while its run is two or more.
 */
export function containerTrouble(markers: readonly Pick<Marker, "kind" | "at" | "server" | "replica" | "exitCode">[], now: number): ContainerTrouble | null {
  const recent = markers.filter((marker) => marker.at > now - TROUBLE_WINDOW_MS).sort((a, b) => a.at - b.at);
  if (recent.some((marker) => marker.kind === "oom")) return "OOM";
  const loops = new Map<string, { crashed: boolean; restarts: number; last: number }>();
  for (const marker of recent) {
    const replica = `${marker.server.id}/${marker.replica}`;
    const loop = loops.get(replica) ?? { crashed: false, restarts: 0, last: 0 };
    if (marker.kind === "exit") {
      if (marker.exitCode !== undefined && marker.exitCode !== 0 && !STOP_SIGNAL_EXITS.has(marker.exitCode)) loops.set(replica, { ...loop, crashed: true });
      else loops.delete(replica);
    } else if (marker.kind === "restart" && loop.crashed) {
      const restarts = marker.at - loop.last > LOOP_QUIET_MS ? 1 : loop.restarts + 1;
      loops.set(replica, { crashed: false, restarts, last: marker.at });
    }
  }
  return [...loops.values()].some((loop) => loop.restarts >= 2 && loop.last > now - LOOP_QUIET_MS) ? "Restarting" : null;
}

/**
 * A node's ⚠: how many things on it the user can fix, red when one is its Service being down. `lead` names the issue the
 * status word doesn't already say (what its containers went through, an unreachable hostname, a failing healthcheck),
 * and the ⚠ shows it ahead of the count. `monitoring` when an open uptime incident or container trouble is among them,
 * so the ⚠ opens Monitoring, where the evidence is.
 */
export type NodeIssues = { count: number; tone: "bad" | "warn"; monitoring: boolean; lead: string | null };

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
  const lead = trouble ?? (uptime ? "Unreachable" : status.checkFailing ? "Check failing" : null);
  return count === 0 ? null : { count, tone: status.down || uptime ? "bad" : "warn", monitoring: own || trouble !== null, lead };
}

/** The public domain a Service's card shows, of its domains: the first custom one, else the generated one; `live` once it's set up. */
export function publicDomain(domains: readonly DomainRow[]) {
  const shown = domains.find((domain) => domain.kind === "custom") ?? domains.find((domain) => domain.kind === "generated" && domain.hostname !== null);
  return shown?.hostname ? { hostname: shown.hostname, live: shown.status !== "setting_up" } : null;
}

export type StagedColour = "changed" | "destructive";

const stagedColour = (change: ReviewLifecycleKind): StagedColour =>
  change === "delete" ? "destructive" : "changed";

/**
 * A node's chip: a Deploy in flight that targets it, running since `since` (Unix seconds) or queued, else what the next
 * Deploy does to it.
 */
export type DeployChipState =
  | { kind: "deploying"; since: number }
  | { kind: "queued" }
  | { kind: "staged"; label: string; variant: StagedColour };

/** What the next Deploy does to a node, in one word. Every staged surface says it, so colour never speaks alone. */
export const stagedWord = (change: ReviewLifecycleKind) =>
  change === "create" ? "New" : change === "delete" ? "Removing" : "Changed";

export function stagedChip(change: ReviewLifecycleKind, changeCount: number): DeployChipState {
  const label = change === "update" && changeCount > 0 ? plural(changeCount, "change") : stagedWord(change);
  return { kind: "staged", label, variant: stagedColour(change) };
}

/**
 * A Service's chip: anything about Deploys. A Deploy in flight that targets it (running, else queued), else what the
 * next Deploy does to it. `inFlight`: the Environment's Deployments in flight; one naming no Services deploys every one.
 * `deploy`: what the running one does to it. Only a Service it restarts reads Deploying, whether or not the Deploy
 * names it.
 */
export function deployChip(
  service: Pick<ServiceListing, "name" | "change">,
  changeCount: number,
  inFlight: readonly Pick<DeploymentSummary, "status" | "services" | "ships" | "started_at" | "admitted_at">[],
  deploy: DeployEffect = "leaves",
): DeployChipState | null {
  const running = runningDeployment(inFlight);
  if (running && deploy === "restarts") return { kind: "deploying", since: running.started_at ?? running.admitted_at };
  if (inFlight.some((deployment) => deployment.status === "queued" && shippedService(deployment, service.name))) return { kind: "queued" };
  return service.change === null ? null : stagedChip(service.change, changeCount);
}

export function runningDeployment<D extends Pick<DeploymentSummary, "status">>(inFlight: readonly D[]): D | null {
  return inFlight.find((deployment) => deployment.status !== "queued") ?? null;
}

/**
 * What the `running` Deploy does to the Service `name`, from its view as last read and whether that read `failed`.
 * Until its runner records the plan every node reads Pending, so only a Service it built a new image for restarts and
 * every other waits: a build still running, or one that reused an image, says nothing of the plan.
 */
export function deployEffect(
  name: string,
  running: Pick<DeploymentSummary, "services" | "ships"> | null,
  { result, failed }: CachedStoreRead<Pick<DeploymentView, "nodes" | "preview" | "builds">>,
): DeployEffect {
  if (running === null || !shippedService(running, name)) return "leaves";
  if (result !== undefined && !result.ok) return "unreadable";
  const effect = result === undefined ? "undecided" : planned(name, result.value);
  return effect === "undecided" && failed ? "unreadable" : effect;
}

function planned(name: string, { nodes, preview, builds }: Pick<DeploymentView, "nodes" | "preview" | "builds">): DeployEffect {
  if (preview === null) return builds.some((build) => build.service === name && build.status === "built") ? "restarts" : "undecided";
  return nodes.some((node) => node.type === "service" && node.name === name && node.outcome !== "unchanged") ? "restarts" : "leaves";
}

/** A node's staged surface, in the colour of what the next Deploy does to it. */
export function stagedSurface(change: ReviewLifecycleKind | null): StagedColour | undefined {
  return change === null ? undefined : stagedColour(change);
}

/** A chip in one word, as a screen reader hears it. */
export function chipWord(chip: DeployChipState | null) {
  return chip === null ? null : chip.kind === "deploying" ? "Deploying" : chip.kind === "queued" ? "Queued" : chip.label;
}

/** The ⚠ as a screen reader hears it: "OOM, 1 issue". */
export const issuesLabel = ({ count, lead }: NodeIssues) => lead ? `${lead}, ${plural(count, "issue")}` : plural(count, "issue");

/**
 * A node's accessible name: its name, then what its card says in words, so a screen reader hears
 * "api, Queued, Can't reach servers" and not the card's text run together.
 */
export const nodeLabel = (name: string, ...said: (string | null | undefined)[]) => [name, ...said.filter(Boolean)].join(", ");

/** A status line in words, with its age when it isn't current ("Online 2 minutes ago"); the shimmer reads Checking. */
export function statusWord(status: Pick<RuntimeLine, "word" | "tone" | "since">, now = new Date()) {
  if (status.tone === "pending") return "Checking";
  return status.since ? `${status.word} ${formatRelativeTime(status.since, now)}` : status.word;
}

/** How many of a Service's secrets have no value, which its Deploys leave out. None when every secret has one. */
export function notSetLabel(notSet: readonly string[]): string | undefined {
  return notSet.length ? `${notSet.length} not set` : undefined;
}
