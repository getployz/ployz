import type {
  ChangeKind, DeploymentStatus, DeploymentSummary, DeploymentView, DiffView, JsonValue, NodeChange, NodeOutcome, NodeStatus, ServiceListing,
  UploadedSource,
} from "@ployz/sdk";
import { Option, Schema } from "effect";
import { settingTitle } from "./catalog";

/** One changed Setting in Details. */
export type ChangeRow = {
  changeKey: string;
  label: string;
  kind: ChangeKind;
  /** Its Store path, which Discard takes. */
  path: string;
  currentValue: string;
  newValue: string;
  canDiscard: boolean;
};

/** One changed node in Details, with its changed Settings. */
export type ChangeGroup = {
  nodeType: NodeChange["type"];
  nodeId: string;
  nodeName: string;
  /** What Discard names to put the whole node back: `SERVICE`, or `volumes.VOLUME`. */
  discardPath: string;
  lifecycle: NodeChange["lifecycle"];
  rows: ChangeRow[];
  changeCount: number;
  canDiscard: boolean;
  serviceSourceType?: ServiceListing["source"];
};

/** A Setting the catalog doesn't title: a variable, a mount, a route, or a Volume's own. */
function untitledLabel(nodeType: NodeChange["type"], setting: string) {
  if (nodeType === "volume") return setting === "node" ? "Volume" : setting === "name" ? "Name" : setting;
  if (setting.startsWith("env.")) return `Environment variable ${setting.slice(4)}`;
  if (setting.startsWith("mounts.")) return `Volume mount ${setting.slice(7)}`;
  if (setting.startsWith("routes.") || setting.startsWith("domains.")) return "Custom domain";
  if (setting === "managedHostnames") return "Generated domain";
  if (setting === "source") return "Source";
  return setting;
}

/**
 * The Store's review as the bottom bar's Details groups it: one group per changed node, one row per changed Setting.
 * Rows discard by their Store path (`web.replicas`), a Service by its name. The Store can't discard a Volume node,
 * so only Services' changes offer Discard.
 */
export function changeGroups(diff: DiffView, services: readonly ServiceListing[]): ChangeGroup[] {
  return diff.changes.map((node) => ({
    nodeType: node.type,
    nodeId: node.id,
    nodeName: node.name,
    discardPath: node.type === "volume" ? `volumes.${node.name}` : node.name,
    lifecycle: node.lifecycle,
    changeCount: Math.max(node.settings.length, 1),
    canDiscard: true,
    serviceSourceType: services.find((service) => service.id === node.id)?.source,
    rows: node.settings.map((row) => {
      // `SERVICE.SETTING`, or `volumes.VOLUME.SETTING`.
      const setting = node.type === "volume" ? row.path.split(".").slice(2).join(".") : row.path.slice(row.path.indexOf(".") + 1);
      const title = settingTitle(setting);
      return {
        changeKey: `${node.id}:${row.path}`,
        path: row.path,
        kind: row.kind,
        label: setting === "name" ? "Name" : title ?? untitledLabel(node.type, setting),
        currentValue: shownValue(row.before),
        newValue: shownValue(row.after),
        // A Setting, variable or mount discards alone; a domain or a rename goes with its node.
        canDiscard: node.type === "service" && (row.canRestore && title !== undefined || /^(env|mounts)\./u.test(setting)),
      };
    }),
  }));
}

/** The diff values a cell words: text, a sealed value (the Store never sends its plaintext), a route. */
const decodeShown = Schema.decodeUnknownOption(Schema.Union([
  Schema.String, Schema.Struct({ secret: Schema.Literal(true) }), Schema.Struct({ hostname: Schema.String }),
  Schema.Struct({ path: Schema.String, timeoutSeconds: Schema.Number }),
  Schema.Array(Schema.Struct({ prefix: Schema.String, targetPort: Schema.NullOr(Schema.Number) })),
  // A whole source, when a Service connects or disconnects one.
  Schema.Struct({ type: Schema.Literals(["image", "git", "empty"]), image: Schema.optional(Schema.String), repository: Schema.optional(Schema.String) }),
]));

/**
 * A diff value as a cell shows it: text as is, a sealed one as Sealed, a route by its hostname, a healthcheck by its
 * path and timeout, generated domains by name and port; else its JSON.
 */
export function shownValue(value: JsonValue): string {
  if (value === null) return "";
  return Option.match(decodeShown(value), {
    onNone: () => JSON.stringify(value),
    onSome: (shown) => {
      if (Schema.is(Schema.String)(shown)) return shown;
      if ("hostname" in shown) return shown.hostname;
      if ("secret" in shown) return "Sealed";
      if ("path" in shown) return `${shown.path} within ${shown.timeoutSeconds}s`;
      if ("type" in shown) return shown.image ?? shown.repository ?? "None";
      return shown.map(({ prefix, targetPort }) => targetPort === null ? prefix : `${prefix} → port ${targetPort}`).join(", ");
    },
  });
}

/** The statuses of a Deployment that holds, or waits for, its Environment's one run. */
export const IN_FLIGHT = ["queued", "running", "cancelling"] as const satisfies readonly DeploymentStatus[];

/** Whether a Deployment holds, or waits for, its Environment's one run. */
export const isInFlight = (status: DeploymentStatus): status is (typeof IN_FLIGHT)[number] =>
  IN_FLIGHT.some((inFlight) => inFlight === status);

const deploymentStatusLabels = {
  queued: "Queued", running: "Deploying", cancelling: "Cancelling", applied: "Deployed", failed: "Failed",
  unknown: "Unknown", cancelled: "Cancelled", superseded: "Superseded",
} satisfies Record<DeploymentStatus, string>;

/**
 * A Deployment as its icon shows it. `unknown`: its runner vanished mid-run, so nobody knows what applied; it never
 * reads as failed or deployed.
 */
export type DeploymentLight = "queued" | "deploying" | "deployed" | "failed" | "unknown" | "cancelled";

/** One node under an open Deployment Page, as its badge, icon and canvas card show it. */
export type NodeLight = "queued" | "deploying" | "deployed" | "failed" | "unknown" | "not_applied";

/** Each status in the icons' vocabulary. */
export const deploymentStatusIcons = {
  queued: "queued", running: "deploying", cancelling: "deploying", applied: "deployed", failed: "failed",
  unknown: "unknown", cancelled: "cancelled", superseded: "cancelled",
} satisfies Record<DeploymentStatus, DeploymentLight>;

export const nodeStatusLabels = {
  pending: "Pending", deployed: "Deployed", removed: "Removed", failed: "Failed", not_attempted: "Not attempted",
  unchanged: "Unchanged", unknown: "Unknown",
} satisfies Record<NodeStatus, string>;

/** Whether the Deployment left the node as intended: deployed, removed, or nothing it had to change. */
export const nodeApplied = (outcome: NodeStatus) => outcome === "deployed" || outcome === "removed" || outcome === "unchanged";

/** A node's outcome as the badges and canvas lighting show it; a pending node reads as its Deployment does. */
export function nodeLight(outcome: NodeStatus, deployment: DeploymentStatus): NodeLight {
  if (nodeApplied(outcome)) return "deployed";
  if (outcome === "failed") return "failed";
  if (outcome === "unknown") return "unknown";
  if (outcome === "pending" && deployment === "queued") return "queued";
  if (outcome === "pending" && isInFlight(deployment)) return "deploying";
  return "not_applied";
}

/**
 * A node's outcome in words: the glossary's Node Outcome once it has one; until then Queued or Deploying, as its
 * Deployment reads, and Not attempted once that ended without it.
 */
export function nodeOutcomeLabel(outcome: NodeStatus, deployment: DeploymentStatus) {
  if (outcome !== "pending") return nodeStatusLabels[outcome];
  if (deployment === "queued") return deploymentStatusLabels.queued;
  return isInFlight(deployment) ? deploymentStatusLabels.running : nodeStatusLabels.not_attempted;
}

/**
 * A Deployment's status in words. One that takes its Environment off the Servers reads as the Branch panel says it:
 * Coming off the servers, then Off the servers.
 */
export function deploymentStatusLabel({ status, remove }: Pick<DeploymentSummary, "status" | "remove">) {
  if (remove && status === "running") return "Coming off the servers";
  if (remove && status === "applied") return "Off the servers";
  return deploymentStatusLabels[status];
}

/**
 * What a Deployment ships, for its row: "Deploys every service", or the Services a targeted Deploy named; nothing for one
 * that takes its Environment off the Servers.
 */
export const deploysLabel = ({ services, remove }: Pick<DeploymentSummary, "services" | "remove">) =>
  remove ? null : `Deploys ${services.length === 0 ? "every service" : services.join(", ")}`;

const time = (seconds: number | null) => seconds === null ? null : new Date(seconds * 1000);

/** Who admitted a Deployment (nobody for the Store's own automation), and when it was admitted, started and ended. */
export function admission(deployment: DeploymentSummary) {
  return {
    by: deployment.admitted_by,
    at: time(deployment.admitted_at),
    started: time(deployment.started_at),
    ended: time(deployment.ended_at),
  };
}

/** Where an upload came from: "Uploaded by nick · abc1234 + changes"; it names no uploader who is `starter`. */
export function uploadLabel(upload: UploadedSource, starter?: string | null) {
  const base = upload.base ? ` · ${upload.base.commit.slice(0, 7)}${upload.base.changed ? " + changes" : ""}` : "";
  return `Uploaded${upload.uploader && upload.uploader !== starter ? ` by ${upload.uploader}` : ""}${base}`;
}

/**
 * Who started a Deployment and what it ships: the `service`'s pinned commit, else the first pinned one, else its
 * upload. "by nick · 8a7ed6e".
 */
export function deploymentByline(deployment: DeploymentView, service: string | undefined) {
  const commit = deployment.builds.find((build) => build.service === service)?.commit ?? deployment.builds.find((build) => build.commit)?.commit;
  const by = deployment.admitted_by;
  return [
    by ? `by ${by}` : null,
    commit ? commit.slice(0, 7) : deployment.upload ? uploadLabel(deployment.upload, by) : null,
  ].filter(Boolean).join(" · ");
}

/** The Service a Deployment page opens on: the picked one, else the one that failed, else one it didn't apply, else the first. */
export function focusedService(deployment: DeploymentView, picked: string | undefined) {
  const services = deployment.nodes.filter((node) => node.type === "service");
  return services.find((node) => node.id === picked) ?? services.find((node) => node.outcome === "failed")
    ?? services.find((node) => !nodeApplied(node.outcome)) ?? services[0];
}

/**
 * Whether a failed Deployment's `node` can be fixed on a Branch: the Deployment ran and `node` didn't apply. With no
 * Server the failure is having none, and adding one is the way on.
 */
export const canFixOnBranch = (deployment: DeploymentView, node: NodeOutcome, noServers: boolean) =>
  !noServers && deployment.status === "failed" && deployment.outcome?.type === "executed" && !nodeApplied(node.outcome);

/**
 * Why `node` has no deploy logs in the Deployment: nothing of it ran, its turn never came, or it was removed. Null when it
 * may have some.
 */
export function missingDeployLogs(deployment: DeploymentView, node: NodeOutcome) {
  if (deployment.started_at === null || deployment.outcome?.type === "not_executed") return "Not started";
  return node.outcome === "not_attempted" || node.outcome === "removed" ? nodeStatusLabels[node.outcome] : null;
}

/** The one action a Deployment page shows by itself. */
export type DeploymentAction = "add_server" | "retry" | "start" | "cancel";

/**
 * Retry an ended Deployment that didn't apply, start a queued one, cancel one before it ends; with no Server, adding
 * one comes first.
 */
function primaryAction(status: DeploymentStatus, noServers: boolean): DeploymentAction | null {
  const retry = status === "failed" || status === "unknown" || status === "cancelled";
  if (noServers && (retry || status === "queued")) return "add_server";
  if (retry) return "retry";
  if (status === "queued") return "start";
  return status === "running" ? "cancel" : null;
}

/** The action a Deployment page shows, and whether Cancel waits in its menu beside another one. */
export function deploymentActions(status: DeploymentStatus, noServers: boolean) {
  const primary = primaryAction(status, noServers);
  return { primary, cancelInMenu: (status === "queued" || status === "running") && primary !== "cancel" };
}
