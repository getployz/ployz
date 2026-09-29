import type { DeploymentStatus, DeploymentSummary, DiffView, JsonValue, NodeStatus, Outcome, ServiceListing, UploadedSource } from "@ployz/sdk";
import { Option, Schema } from "effect";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import type { DeploymentNodeView, DeploymentViewStatus } from "#/modules/deployments/deployment-view";
import { presentSettingChange } from "#/modules/services/service-deployment-diff/fields";
import { settingTitle } from "./catalog";

/**
 * The Store's review as the bottom bar's Details groups it: one group per changed node, one row per changed Setting.
 * Rows discard by their Store path (`web.replicas`), a Service by its name. The Store can't discard a Volume node,
 * so only Services' changes offer Discard.
 */
export function changeGroups(diff: DiffView, services: readonly ServiceListing[]): CanvasEnvironmentChangeGroup[] {
  return diff.changes.map((node) => ({
    nodeType: node.type,
    nodeId: node.id,
    nodeName: node.name,
    summaryLabel: "",
    lifecycle: node.lifecycle,
    changeCount: Math.max(node.settings.length, 1),
    canDiscard: node.type === "service",
    serviceSourceType: services.find((service) => service.id === node.id)?.source,
    rows: node.settings.map((row) => {
      // `SERVICE.SETTING`, or `volumes.VOLUME.SETTING`.
      const setting = node.type === "volume" ? row.path.split(".").slice(2).join(".") : row.path.slice(row.path.indexOf(".") + 1);
      const title = settingTitle(setting);
      return {
        changeKey: `${node.id}:${row.path}`,
        path: row.path,
        kind: row.kind,
        label: setting === "name" ? "Name" : title ?? presentSettingChange(node.type, setting, row.before, row.after).label,
        currentValue: shownValue(row.before),
        newValue: shownValue(row.after),
        // Only a catalog Setting discards alone; a variable, mount, domain or rename goes with its Service.
        canDiscard: node.type === "service" && row.canRestore && title !== undefined,
      };
    }),
  }));
}

/** The diff values a cell words: text, a sealed value (the Store never sends its plaintext), a route. */
const decodeShown = Schema.decodeUnknownOption(Schema.Union([
  Schema.String, Schema.Struct({ secret: Schema.Literal(true) }), Schema.Struct({ hostname: Schema.String }),
]));

/** A diff value as a cell shows it: text as is, a sealed one as Sealed, a route by its hostname, else its JSON. */
function shownValue(value: JsonValue): string {
  if (value === null) return "";
  return Option.match(decodeShown(value), {
    onNone: () => JSON.stringify(value),
    onSome: (shown) => Schema.is(Schema.String)(shown) ? shown : "secret" in shown ? "Sealed" : shown.hostname,
  });
}

/** Whether a Deployment holds, or waits for, its Environment's one run. */
export const isInFlight = (status: DeploymentStatus) => status === "queued" || status === "running" || status === "cancelling";

export const deploymentStatusLabels = {
  queued: "Queued", running: "Deploying", cancelling: "Cancelling", applied: "Deployed", failed: "Failed",
  unknown: "Unknown", cancelled: "Cancelled", superseded: "Superseded",
} satisfies Record<DeploymentStatus, string>;

/** Each status in the icons' vocabulary. `unknown` (its runner vanished mid-run) needs a look, like a failure. */
export const deploymentStatusIcons = {
  queued: "queued", running: "deploying", cancelling: "deploying", applied: "deployed", failed: "failed",
  unknown: "failed", cancelled: "cancelled", superseded: "cancelled",
} satisfies Record<DeploymentStatus, DeploymentViewStatus>;

export const nodeStatusLabels = {
  pending: "Pending", applied: "Applied", not_applied: "Not applied", unknown: "Unknown",
} satisfies Record<NodeStatus, string>;

/** A node's outcome in the badges' and canvas lighting's vocabulary; a pending node reads as its Deployment does. */
export function nodeLight(outcome: NodeStatus, deployment: DeploymentStatus): DeploymentNodeView["outcome"] {
  if (outcome === "applied") return "deployed";
  if (outcome === "unknown") return "failed";
  if (outcome === "pending" && deployment === "queued") return "queued";
  if (outcome === "pending" && isInFlight(deployment)) return "deploying";
  return "not_attempted";
}

/** "every service", or the Services a targeted Deploy named. */
export const targetsLabel = (deployment: Pick<DeploymentSummary, "services">) =>
  deployment.services.length === 0 ? "every service" : deployment.services.join(", ");

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

/** Why a Deployment didn't run, and the CLI line that gives it an upload it needs. */
export function notExecuted(outcome: Outcome | null) {
  if (outcome?.type !== "not_executed") return null;
  return { reason: outcome.reason, next: outcome.needs_upload.length ? "ployz deploy --upload ." : null };
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
  const plural = (n: number, noun: string) => `${n} ${noun}${n === 1 ? "" : "s"}`;
  return [
    operations.length === 0 ? "Nothing to change" : `${plural(operations.length, "operation")}: ${[...counts].map(([name, n]) => `${name} ${n}`).join(", ")}`,
    ...(volumes_to_create.length ? [`Creates ${plural(volumes_to_create.length, "volume")}`] : []),
    ...(would_remove.length ? [`Removes ${plural(would_remove.length, "service")}`] : []),
    ...warnings.map((warning) => warning.message ?? warning.type.replaceAll("_", " ")),
  ];
}
