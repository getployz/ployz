import type {
  ChangeKind, DeploymentStatus, DeploymentSummary, DiffView, JsonValue, NodeChange, NodeStatus, Outcome, ServiceListing, UploadedSource,
} from "@ployz/sdk";
import { Option, Schema } from "effect";
import { plural } from "#/lib/plural";
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
]));

/**
 * A diff value as a cell shows it: text as is, a sealed one as Sealed, a route by its hostname, a healthcheck by its
 * path and timeout, generated domains by name and port; else its JSON.
 */
function shownValue(value: JsonValue): string {
  if (value === null) return "";
  return Option.match(decodeShown(value), {
    onNone: () => typeof value === "object" ? JSON.stringify(value) : String(value),
    onSome: (shown) => {
      if (Schema.is(Schema.String)(shown)) return shown;
      if ("hostname" in shown) return shown.hostname;
      if ("secret" in shown) return "Sealed";
      if ("path" in shown) return `${shown.path} within ${shown.timeoutSeconds}s`;
      return shown.map(({ prefix, targetPort }) => targetPort === null ? prefix : `${prefix} → port ${targetPort}`).join(", ");
    },
  });
}

/** The statuses of a Deployment that holds, or waits for, its Environment's one run. */
export const IN_FLIGHT = ["queued", "running", "cancelling"] as const satisfies readonly DeploymentStatus[];

/** Whether a Deployment holds, or waits for, its Environment's one run. */
export const isInFlight = (status: DeploymentStatus): status is (typeof IN_FLIGHT)[number] =>
  IN_FLIGHT.some((inFlight) => inFlight === status);

export const deploymentStatusLabels = {
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

export const nodeLightLabels = {
  queued: "Queued", deploying: "Deploying", deployed: "Deployed", failed: "Failed", unknown: "Unknown", not_applied: "Not applied",
} satisfies Record<NodeLight, string>;

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

/** "every service", or the Services a targeted Deploy named. */
export const targetsLabel = (deployment: Pick<DeploymentSummary, "services">) =>
  deployment.services.length === 0 ? "every service" : deployment.services.join(", ");

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

/** Where an upload came from: "Uploaded by nick · abc1234 + changes". */
export function uploadLabel(upload: UploadedSource) {
  const base = upload.base ? ` · ${upload.base.commit.slice(0, 7)}${upload.base.changed ? " + changes" : ""}` : "";
  return `Uploaded${upload.uploader ? ` by ${upload.uploader}` : ""}${base}`;
}

/** What the user can do with a Deployment: retry an ended one that didn't apply, start a queued one, cancel one before it ends. */
export function deploymentActions(status: DeploymentStatus) {
  return {
    retry: status === "failed" || status === "unknown" || status === "cancelled",
    start: status === "queued",
    cancel: status === "queued" || status === "running",
  };
}

/** Why a Deployment didn't run, and the Services that had nothing to run (each needs an image or a repository). */
export function notExecuted(outcome: Outcome | null) {
  if (outcome?.type !== "not_executed") return null;
  return { reason: outcome.reason, needsSource: outcome.needs_upload };
}

/** What a failed step's kind means, in plain words, when the runner gave no message of its own. */
const FAILURE_WORDS = {
  machine: "A Server couldn't carry out a step.",
  health: "A new container failed its health check.",
  dependency_health: "A Service it depends on isn't healthy.",
  hook: "The pre-deploy command failed.",
  cancelled: "It was cancelled.",
} as const;
const FailedSummary = Schema.Struct({
  type: Schema.Literal("failed"),
  reason: Schema.Literals(["machine", "health", "dependency_health", "hook", "cancelled"]),
  message: Schema.optional(Schema.NullOr(Schema.String)),
});
const decodeFailed = Schema.decodeUnknownOption(FailedSummary);

/** Why an executed Deployment failed, from its recorded outcome: the runner's message, else its step's kind. */
export function failureReason(outcome: Outcome | null) {
  if (outcome?.type !== "executed") return null;
  return Option.match(decodeFailed(outcome.summary), {
    onNone: () => null,
    onSome: (failed) => failed.message || FAILURE_WORDS[failed.reason],
  });
}

/** The part of a recorded Deploy Preview the page shows; the Store keeps the rest. */
const PreviewSummary = Schema.Struct({
  operations: Schema.Array(Schema.Struct({ service_name: Schema.NullOr(Schema.String) })),
  would_remove: Schema.Array(Schema.Unknown),
  volumes_to_create: Schema.Array(Schema.Unknown),
  warnings: Schema.Array(Schema.Struct({ type: Schema.String, message: Schema.optional(Schema.String) })),
});
const decodePreview = Schema.decodeUnknownOption(PreviewSummary);

/**
 * The Deploy Preview its runner recorded before executing, in a few lines: operations per Service, what it creates
 * and removes, and its warnings. Null before a runner prepared one.
 */
export function previewLines(preview: JsonValue | null): string[] | null {
  const decoded = decodePreview(preview);
  if (Option.isNone(decoded)) return null;
  const { operations, would_remove, volumes_to_create, warnings } = decoded.value;
  const counts = new Map<string, number>();
  for (const { service_name } of operations) counts.set(service_name ?? "Environment", (counts.get(service_name ?? "Environment") ?? 0) + 1);
  return [
    operations.length === 0 ? "Nothing to change" : `${plural(operations.length, "operation")}: ${[...counts].map(([name, n]) => `${name} ${n}`).join(", ")}`,
    ...(volumes_to_create.length ? [`Creates ${plural(volumes_to_create.length, "volume")}`] : []),
    ...(would_remove.length ? [`Removes ${plural(would_remove.length, "service")}`] : []),
    ...warnings.map((warning) => warning.message ?? warning.type.replaceAll("_", " ")),
  ];
}
