import type { DeploymentSummary, DomainRow, ReviewLifecycleKind, ServiceListing } from "@ployz/sdk";
import { plural } from "#/lib/plural";
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
 */
export type RuntimeLine = { word: string; tone: Tone; down: boolean; since: Date | null };

const line = (word: string, tone: Tone, down = false): RuntimeLine => ({ word, tone, down, since: null });

/** A Volume's status line while nothing here mounts it. */
export const NOT_MOUNTED = line("Not mounted", "idle");

/** What evidence says of a Service (`runtime`, null when none names it; `whole`, no Server is missing from it). */
function evidenceLine(runtime: Pick<RuntimeServiceRecord, "containers"> | null, whole: boolean, desiredReplicas: number | null, deploying: boolean) {
  if (!runtime) return whole ? line("Not running", "bad", true) : line("Deployed", "quiet");
  if (runtime.containers.length === 0) return line("Not running", "bad", true);
  const running = runtime.containers.filter((container) => container.runtime?.state === "running");
  if (running.length === 0) return line("Crashed", "crashed", true);
  const serving = running.filter(containerServing).length;
  // None serves yet: Unhealthy once a health check fails, else still Starting.
  if (serving === 0) return running.some((container) => container.runtime?.health === "unhealthy") ? line("Unhealthy", "warn") : line("Starting", "quiet");
  if (!deploying && desiredReplicas !== null && serving < desiredReplicas) return line("Degraded", "warn");
  return line("Online", "ok");
}

/**
 * What the Runtime Watch says of the Servers, as `useRuntimeLens` reads it once for the canvas: its status, whether a
 * Server is missing from its evidence, when that was current, and whether there is no Server at all.
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
  if (service.change === "create") return line("Not deployed", "idle");
  if (service.source === "empty") return line("No source", "idle");
  if (lens.noServers) return line("Needs a server", "idle");
  if (lens.status === "unreachable") return line("Can't reach servers", "unreachable");
  if (lens.status === "observed") return evidenceLine(runtime, !lens.incomplete, desiredReplicas, deploying);
  // The connection dropped: the last evidence, grey, from when it was current.
  if (lens.status === "unavailable" && lens.observedAt !== null) {
    return { ...evidenceLine(runtime, !lens.incomplete, desiredReplicas, deploying), tone: "quiet", down: false, since: new Date(lens.observedAt) };
  }
  // Connecting, or no evidence was ever seen: it waits.
  return line("Checking", "pending");
}

/** A node's ⚠ N: how many things on it the user can fix, red when one is its Service being down. */
export type NodeIssues = { count: number; tone: "bad" | "warn" };

/** What on a node the user can fix: a Service that's down or struggling, and domains that need them. */
export function nodeIssues(status: RuntimeLine, domains: readonly Pick<DomainRow, "status">[]): NodeIssues | null {
  const count = (status.down || status.tone === "warn" ? 1 : 0) + domains.filter((domain) => domain.status === "needs_attention").length;
  return count === 0 ? null : { count, tone: status.down ? "bad" : "warn" };
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
 */
export function deployChip(
  service: Pick<ServiceListing, "name" | "change">,
  changeCount: number,
  inFlight: readonly Pick<DeploymentSummary, "status" | "services" | "started_at" | "admitted_at">[],
): DeployChipState | null {
  const targeting = inFlight.filter((deployment) => deployment.services.length === 0 || deployment.services.includes(service.name));
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
