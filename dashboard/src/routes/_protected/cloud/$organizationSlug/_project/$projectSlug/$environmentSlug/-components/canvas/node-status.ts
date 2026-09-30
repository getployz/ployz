import type { DeploymentSummary, DomainRow, ReviewLifecycleKind, ServiceListing } from "@ployz/sdk";
import type { RuntimeLensStatus, RuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { containerServing } from "#/routes/_protected/cloud/$organizationSlug/-components/services-online";

/**
 * How much the canvas can say about what runs: `live`, the Servers' whole evidence; `partial`, some Server didn't
 * report, so an absence proves nothing; `stale`, the connection dropped and the last evidence (from `since`) is kept;
 * `connecting` before any; `unreachable`, the Servers can't be reached and nothing is kept; `no_servers`, there are none.
 */
export type Evidence =
  | { kind: "live" | "partial" | "connecting" | "unreachable" | "no_servers" }
  | { kind: "stale"; since: Date | null };

export function runtimeEvidence(status: RuntimeLensStatus, incomplete: boolean, observedAt: string | null): Evidence {
  switch (status) {
    case "observed":
      return { kind: incomplete ? "partial" : "live" };
    case "unavailable":
      return { kind: "stale", since: observedAt === null ? null : new Date(observedAt) };
    case "no_connection":
      return { kind: "no_servers" };
    case "connecting":
    case "unreachable":
      return { kind: status };
  }
}

/**
 * How a status line reads: its colour; `idle`, a hollow dot for a Service with nothing to run yet; `quiet`, grey, for
 * evidence that isn't current; `pending`, a shimmer while the first evidence comes in; `unreachable`, a cloud-off icon.
 */
export type Tone = "ok" | "warn" | "bad" | "quiet" | "idle" | "pending" | "unreachable";

/**
 * What runs now, in one word. `down`: it should serve and doesn't, which turns the card's border red.
 * `since`: when the evidence behind a grey word was current ("Online 2 minutes ago").
 */
export type RuntimeLine = { word: string; tone: Tone; down: boolean; since: Date | null };

const line = (word: string, tone: Tone, down = false): RuntimeLine => ({ word, tone, down, since: null });

/** What current evidence says of a Service (`runtime`, null when none names it; `whole`, no Server is missing from it). */
function evidenceLine(runtime: Pick<RuntimeServiceRecord, "containers"> | null, whole: boolean, desiredReplicas: number | null, deploying: boolean) {
  if (!runtime) return whole ? line("Not running", "bad", true) : line("Deployed", "quiet");
  if (runtime.containers.length === 0) return line("Not running", "bad", true);
  if (!runtime.containers.some((container) => container.runtime?.state === "running")) return line("Crashed", "bad", true);
  const serving = runtime.containers.filter(containerServing).length;
  if (serving === 0) return line("Unhealthy", "warn");
  if (!deploying && desiredReplicas !== null && serving < desiredReplicas) return line("Degraded", "warn");
  return line("Online", "ok");
}

/**
 * A Service's status line: what runs now, from runtime evidence. Staged work and Deploys never replace it, and it never
 * guesses: before evidence it waits, and evidence that isn't current reads grey with its age.
 * `desiredReplicas`: how many it asks for, when known; fewer serving reads Degraded, except while a Deploy rolls them.
 */
export function runtimeLine(
  service: Pick<ServiceListing, "change" | "source">,
  runtime: Pick<RuntimeServiceRecord, "containers"> | null,
  { evidence, uploaded, desiredReplicas, deploying }: { evidence: Evidence; uploaded: boolean; desiredReplicas: number | null; deploying: boolean },
): RuntimeLine {
  if (service.change === "create") return line("Not deployed", "idle");
  if (service.source === "empty" && !uploaded) return line("No source", "idle");
  switch (evidence.kind) {
    case "no_servers":
      return line("Needs a server", "idle");
    case "unreachable":
      return line("Can't reach servers", "unreachable");
    case "connecting":
      return line("Checking", "pending");
    case "stale":
      return { ...evidenceLine(runtime, false, desiredReplicas, deploying), tone: "quiet", down: false, since: evidence.since };
    case "partial":
    case "live":
      return evidenceLine(runtime, evidence.kind === "live", desiredReplicas, deploying);
  }
}

/** What on a node the user can fix, as its ⚠ N: a Service that's down or struggling, and domains that need them. Red when it's down. */
export function nodeIssues(status: RuntimeLine, domains: readonly Pick<DomainRow, "status">[]) {
  const count = (status.tone === "bad" || status.tone === "warn" ? 1 : 0)
    + domains.filter((domain) => domain.status === "needs_attention").length;
  return count === 0 ? null : { count, tone: status.tone === "bad" ? "bad" as const : "warn" as const };
}

/** The public domain a Service's card shows: its first custom domain, else its generated one; `live` once it serves. */
export function publicDomain(domains: readonly DomainRow[], service: string) {
  const own = domains.filter((domain) => domain.service === service);
  const shown = own.find((domain) => domain.kind === "custom") ?? own.find((domain) => domain.kind === "generated" && domain.hostname !== null);
  return shown?.hostname ? { hostname: shown.hostname, live: shown.status === "ready" } : null;
}

/** What the next Deploy does to a node, as its chip says it. */
export function stagedChip(change: ReviewLifecycleKind, changeCount: number) {
  if (change === "create") return { kind: "staged", label: "New", variant: "changed" } as const;
  if (change === "delete") return { kind: "staged", label: "Removing", variant: "destructive" } as const;
  return { kind: "staged", label: changeCount === 0 ? "Changed" : `${changeCount} ${changeCount === 1 ? "change" : "changes"}`, variant: "changed" } as const;
}

/**
 * A Service's chip: anything about Deploys. A Deploy in flight that targets it (running, else queued), else what the
 * next Deploy does to it. `inFlight`: the Environment's Deployments in flight; one naming no Services deploys every one.
 */
export function deployChip(
  service: Pick<ServiceListing, "name" | "change">,
  changeCount: number,
  inFlight: readonly Pick<DeploymentSummary, "status" | "services" | "started_at" | "admitted_at">[],
) {
  const targeting = inFlight.filter((deployment) => deployment.services.length === 0 || deployment.services.includes(service.name));
  const running = targeting.find((deployment) => deployment.status !== "queued");
  if (running) return { kind: "deploying", since: running.started_at ?? running.admitted_at } as const;
  if (targeting.length > 0) return { kind: "queued" } as const;
  return service.change === null ? null : stagedChip(service.change, changeCount);
}
