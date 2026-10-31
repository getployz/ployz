import type {
  ChangeKind, DiscardTarget, DeploymentStatus, DeploymentSummary, DeploymentView, DeploymentWarning, DiffView, JsonValue, NodeChange, NodeOutcome, NodeStatus, Outcome, RowId,
  ServiceListing,
} from "@ployz/sdk";
import { Option, Schema } from "effect";
import { settingTitle } from "./catalog";
import { healthcheckOf } from "./healthcheck";
import { branchText, sourceChange } from "./store-branches";
import { volumeStorageText } from "./store-volumes";

/** A Config file in a change. `content` is missing where the reader may not see it: Ployz support, unless the Organization shows it. */
const reviewFile = Schema.Struct({ content: Schema.optional(Schema.String), mode: Schema.String, uid: Schema.Number, gid: Schema.Number });
const decodeFile = Schema.decodeUnknownOption(reviewFile);

/** One changed Setting in Details. */
export type ChangeRow = {
  changeKey: string;
  label: string;
  /** Its name beside its node's: a variable's key, else `label`. */
  name: string;
  variable: boolean;
  kind: ChangeKind;
  /** Its Store path, which Discard takes. */
  path: string;
  /** The Sync row it falls in: what hints, arrivals and Never sync join it by. */
  row: RowId | null;
  currentValue: string;
  newValue: string;
  configFile?: { before: typeof reviewFile.Type | null; after: typeof reviewFile.Type | null };
  canDiscard: boolean;
  discardTarget?: DiscardTarget;
  /** A placement change's second line, in words, where a Setting shows its before and after. */
  detail?: string;
  /** What the change loses, which Details shows in red. */
  loss?: string;
  /** A placement change's steps, in the order the Deploy runs them. */
  steps?: readonly string[];
};

/** One changed node in Details, with its changed Settings. */
export type ChangeGroup = {
  /** Who keeps the change and takes its Discard: the Config Store, or the dashboard's Volume placement. */
  owner: "store" | "placement";
  nodeType: NodeChange["type"];
  nodeId: string;
  nodeName: string;
  /** What Discard names to put the whole node back: `SERVICE`, or `volumes.VOLUME`. */
  discardPath: string;
  /** The node's own Sync row; none for a placement change, which never syncs. */
  row: RowId | null;
  lifecycle: NodeChange["lifecycle"];
  rows: ChangeRow[];
  changeCount: number;
  restarts: NodeChange["restarts"];
  canDiscard: boolean;
  discardTarget?: DiscardTarget;
  serviceSourceType?: ServiceListing["source"];
  /** A failed Deploy gave its changes back before it got to them. */
  unreached?: boolean;
  /** Created only against the Saved version: the deployed one still holds it, so History brings it back. */
  comesBack?: true;
};

/** A Setting the catalog doesn't title: a variable, a mount, a route, or a Volume's own. */
function untitledLabel(nodeType: NodeChange["type"], setting: string) {
  if (nodeType === "volume") return setting === "node" ? "Volume" : setting === "name" ? "Name" : setting;
  if (nodeType === "config") return setting === "name" ? "Name" : setting.startsWith("files.") ? setting.slice("files.".length) : setting;
  if (setting.startsWith("env.")) return `Environment variable ${setting.slice(4)}`;
  if (setting.startsWith("mounts.")) return `Volume mount ${setting.slice(7)}`;
  if (setting.startsWith("configs.")) return `Config mount ${setting.slice(8)}`;
  if (setting.startsWith("routes.") || setting.startsWith("domains.")) return "Custom domain";
  if (setting === "managedHostnames") return "Generated domain";
  if (setting === "source") return "Source";
  return setting;
}

/**
 * The Store's review as the bottom bar's Details groups it: one group per changed node, one row per changed Setting.
 * Rows discard by their Store path (`web.replicas`), a Service by its name, a Volume by `volumes.VOLUME`.
 */
export function changeGroups(diff: DiffView, services: readonly ServiceListing[]): ChangeGroup[] {
  return diff.changes.map((node) => ({
    owner: "store",
    nodeType: node.type,
    nodeId: node.id,
    nodeName: node.name,
    discardPath: node.type === "volume" ? `volumes.${node.name}` : node.type === "config" ? `configs.@${node.id}` : node.name,
    row: node.row,
    lifecycle: node.lifecycle,
    changeCount: node.settings.length + (node.lifecycle === "update" ? 0 : 1),
    restarts: node.restarts,
    canDiscard: true,
    serviceSourceType: services.find((service) => service.id === node.id)?.source,
    rows: node.settings.map((row) => {
      const prefix = node.type === "volume" ? `volumes.${node.name}.` : node.type === "config" ? `configs.@${node.id}.` : `${node.name}.`;
      const setting = row.path.slice(prefix.length);
      const title = settingTitle(setting);
      const label = row.configName ? `Config mount ${row.configName}`
        : setting.startsWith("configs.@") ? "Config mount"
        : setting === "name" ? "Name" : setting === "source.branch" ? "Branch" : title ?? untitledLabel(node.type, setting);
      const variable = node.type === "service" && setting.startsWith("env.");
      const [before, after] = setting === "source" ? sourceChange(row.before, row.after)
        : setting === "source.branch" ? [branchText(row.before), branchText(row.after)]
        : setting === "memLimit" ? [memorySize(row.before), memorySize(row.after)]
        : setting === "cpuLimit" ? [cpuCount(row.before), cpuCount(row.after)] : [shownValue(row.before), shownValue(row.after)];
      const file = (value: JsonValue) => value === null ? null : Option.getOrThrowWith(decodeFile(value),
        () => new Error(`Could not read Config file comparison for ${row.path}.`));
      const configFile = node.type === "config" && setting.startsWith("files.") ? { before: file(row.before), after: file(row.after) } : undefined;
      return {
        changeKey: `${node.id}:${row.path}`,
        path: row.path,
        row: row.row,
        kind: row.kind,
        label,
        name: variable ? setting.slice("env.".length) : label,
        variable,
        currentValue: configFile ? configFile.before === null ? "" : "File" : before,
        newValue: configFile ? configFile.after === null ? "" : "File" : after,
        configFile,
        // Whether Discard takes this path alone is the Store's to say.
        canDiscard: row.canRestore,
      };
    }),
  }));
}

/** Runtime changes and draft reversals in one review, with each row's Discard baseline. */
export function reviewGroups(diff: DiffView, services: readonly ServiceListing[]): ChangeGroup[] {
  const groups = changeGroups(diff, services).map((group) => ({ ...group, rows: [...group.rows] }));
  const drafts = changeGroups({ ...diff, changes: diff.draft_changes ?? [] }, services);
  for (const draft of drafts) {
    draft.discardTarget = "saved";
    draft.rows = draft.rows.map((row) => ({ ...row, discardTarget: "saved" }));
    const runtime = groups.find((group) => group.nodeType === draft.nodeType && group.nodeId === draft.nodeId);
    if (!runtime) {
      groups.push(draft.lifecycle === "create" ? { ...draft, comesBack: true } : draft);
      continue;
    }
    if (runtime.lifecycle !== "update") continue;
    runtime.rows.push(...draft.rows.filter((row) => !runtime.rows.some((other) => sameReviewRow(other, row))));
    if (draft.lifecycle !== "update") {
      runtime.lifecycle = draft.lifecycle;
      runtime.discardTarget = "saved";
      if (draft.lifecycle === "create") runtime.comesBack = true;
    }
    runtime.changeCount = runtime.rows.length + (runtime.lifecycle === "update" ? 0 : 1);
  }
  return groups.sort(reviewOrder);
}

const kindRank = { service: 0, volume: 1, config: 2 } satisfies Record<NodeChange["type"], number>;
const stepRank = { delete: 0, update: 1, create: 2 } satisfies Record<NodeChange["lifecycle"], number>;
const ascending = (a: number | string, b: number | string) => (a < b ? -1 : a > b ? 1 : 0);

/** The Store's review order: Services, Volumes, then Configs, each by name; a removal before what took its name. */
const reviewOrder = (a: ChangeGroup, b: ChangeGroup) =>
  ascending(kindRank[a.nodeType], kindRank[b.nodeType]) || ascending(a.nodeName, b.nodeName)
  || ascending(stepRank[a.lifecycle], stepRank[b.lifecycle]) || ascending(a.nodeId, b.nodeId);

function sameReviewRow(left: ChangeRow, right: ChangeRow): boolean {
  if (!left.row || !right.row) return left.path === right.path;
  if (left.row !== right.row) return false;
  if (left.variable) {
    const field = (row: ChangeRow) => row.path.endsWith(".exported") ? "exported"
      : row.path.endsWith(".description") ? "description" : "value";
    return field(left) === field(right);
  }
  if (left.row.endsWith(":routes")) return left.path.slice(left.path.indexOf(".")) === right.path.slice(right.path.indexOf("."));
  return true;
}

/** The diff values a cell words: text, a sealed value (the Store never sends its plaintext) or one still unset, a route. */
const decodeShown = Schema.decodeUnknownOption(Schema.Union([
  Schema.String, Schema.Struct({ secret: Schema.Boolean }), Schema.Struct({ hostname: Schema.String }),
  Schema.Array(Schema.Struct({ prefix: Schema.String, targetPort: Schema.NullOr(Schema.Number) })),
  // A Volume's storage.
  Schema.Struct({ kind: Schema.Literal("provisioned"), maximumBytes: Schema.Number }), Schema.Struct({ kind: Schema.Literal("docker") }),
]));

/** A memory limit in GB as people read it, in the unit it was likely given: `192 MiB`, `1 GiB`, `0.5 GB`. */
function memorySize(value: JsonValue): string {
  if (!Schema.is(Schema.Number)(value)) return shownValue(value);
  const bytes = Math.round(value * 1e9);
  const [amount, unit] = bytes % 2 ** 30 === 0 ? [bytes / 2 ** 30, "GiB"] : bytes % 2 ** 20 === 0 ? [bytes / 2 ** 20, "MiB"] : [bytes / 1e9, "GB"];
  return `${amount} ${unit}`;
}

/** A CPU limit in vCPUs, as the drawer's field takes it: `0.5 vCPUs`, `1 vCPU`. */
function cpuCount(value: JsonValue): string {
  if (!Schema.is(Schema.Number)(value)) return shownValue(value);
  return `${value} ${value === 1 ? "vCPU" : "vCPUs"}`;
}

/**
 * A diff value as a cell shows it: text as is, a sealed one as Sealed or needing a value, a route by its hostname, a
 * healthcheck by its path or command and timeout, generated domains by name and port, a Volume's storage by its limit;
 * else its JSON.
 */
export function shownValue(value: JsonValue): string {
  if (value === null) return "";
  const check = healthcheckOf(value);
  if (check) return `${check.text} within ${check.timeoutSeconds}s`;
  return Option.match(decodeShown(value), {
    onNone: () => JSON.stringify(value),
    onSome: (shown) => {
      if (Schema.is(Schema.String)(shown)) return shown;
      if ("hostname" in shown) return shown.hostname;
      if ("secret" in shown) return shown.secret ? "Sealed" : "Not set";
      if ("kind" in shown) return volumeStorageText(shown);
      return shown.map(({ prefix, targetPort }) => targetPort === null ? prefix : `${prefix} → port ${targetPort}`).join(", ");
    },
  });
}

/** The Deployment as a Cancel the Store hasn't answered shows it: Cancelling, while it still runs or waits. */
export const showCancelling = <D extends Pick<DeploymentSummary, "status" | "in_flight">>(deployment: D, cancelling: boolean): D =>
  cancelling && deployment.in_flight ? { ...deployment, status: "cancelling" } : deployment;

const deploymentStatusLabels = {
  waiting_for_ci: "Waiting for CI", queued: "Queued", running: "Deploying", cancelling: "Cancelling", applied: "Deployed", failed: "Failed",
  unknown: "Unknown", cancelled: "Cancelled", superseded: "Superseded",
} satisfies Record<DeploymentStatus, string>;

/**
 * A Deployment as its icon shows it. `unknown`: its runner vanished mid-run, so nobody knows what applied; it never
 * reads as failed or deployed.
 */
export type DeploymentLight = "queued" | "deploying" | "deployed" | "failed" | "unknown" | "cancelled";

/** A build's node on its Run page, as `?job=` names it; links into a Run use it to open the build. */
export const buildNodeId = (service: string) => `build:${service}`;

/** One node under an open Deployment Page, as its badge, icon and canvas card show it. */
export type NodeLight = "queued" | "deploying" | "deployed" | "failed" | "unknown" | "not_applied";

/** Each status in the icons' vocabulary. */
const deploymentStatusIcons = {
  waiting_for_ci: "queued", queued: "queued", running: "deploying", cancelling: "deploying", applied: "deployed", failed: "failed",
  unknown: "unknown", cancelled: "cancelled", superseded: "cancelled",
} satisfies Record<DeploymentStatus, DeploymentLight>;

/** What a Deployment's CI says while it waits for it; a summary the Store wrote before it said so has none. */
type WaitingCi = Partial<Pick<DeploymentSummary, "ci">>;

/** A Deployment as its icon shows it: one waiting for CI that failed reads as failed. */
export const deploymentLight = ({ status, ci }: Pick<DeploymentSummary, "status"> & WaitingCi): DeploymentLight =>
  ci === "failed" ? "failed" : deploymentStatusIcons[status];

/**
 * A node's outcome as users read it: Failed work the Deployment's cancel stopped reads Cancelled. The Store marks it
 * rather than giving it a status of its own, so a reader from before Cancelled reads Failed.
 */
export type ShownNodeStatus = NodeStatus | "cancelled";

export const shownNodeStatus = ({ outcome, cancelled }: Pick<NodeOutcome, "outcome" | "cancelled">): ShownNodeStatus =>
  outcome === "failed" && cancelled ? "cancelled" : outcome;

export const nodeStatusLabels = {
  pending: "Pending", deployed: "Deployed", removed: "Removed", failed: "Failed", cancelled: "Cancelled",
  not_attempted: "Not attempted", unchanged: "Unchanged", unknown: "Unknown",
} satisfies Record<ShownNodeStatus, string>;

/** Whether the Deployment left the node as intended: deployed, removed, or nothing it had to change. */
export const nodeApplied = (outcome: ShownNodeStatus) => outcome === "deployed" || outcome === "removed" || outcome === "unchanged";

/** A node's outcome as the badges and canvas lighting show it; a pending node reads as its Deployment does. */
export function nodeLight(outcome: ShownNodeStatus, deployment: Pick<DeploymentSummary, "status" | "in_flight">): NodeLight {
  if (nodeApplied(outcome)) return "deployed";
  if (outcome === "failed") return "failed";
  if (outcome === "unknown") return "unknown";
  if (outcome === "pending" && deployment.status === "queued") return "queued";
  if (outcome === "pending" && deployment.in_flight) return "deploying";
  return "not_applied";
}

/**
 * A Deployment's status in words. One that takes its Environment off the Servers reads as the Branch panel says it:
 * Coming off the servers, then Off the servers; one that completed with no Server left to take it off (`forgotten`)
 * says what it left. One waiting for CI says so, or that its CI failed. One running without a Deploy Preview yet is
 * Preparing: it unpacks its upload, builds and plans.
 */
export function deploymentStatusLabel(
  { status, remove, outcome, ci, preparing }: Pick<DeploymentSummary, "status" | "remove" | "outcome" | "preparing"> & WaitingCi,
) {
  if (ci === "failed") return "CI failed";
  if (outcome?.type === "forgotten") return "Left on old servers";
  if (remove && status === "running") return "Coming off the servers";
  if (remove && status === "applied") return "Off the servers";
  if (preparing && status === "running") return "Preparing";
  return deploymentStatusLabels[status];
}

/**
 * How a Deployment is named: "#N" once it queued; while it waits for CI it has no number yet, so the push's short sha
 * stands in, and the ID's first 8 characters when no push is known.
 */
export const deploymentLabel = ({ number, push, id }: Pick<DeploymentSummary, "number" | "id"> & { push?: { head: string } | null }) =>
  number !== null ? `#${number}` : (push?.head.slice(0, 7) ?? id.slice(0, 8));

/** Whether a Deployment ships Service `name`: a full Deploy ships every one, a narrowed one those it names and those they reference. */
export const shippedService = ({ services, ships }: Pick<DeploymentSummary, "services" | "ships">, name: string) =>
  services.length === 0 || services.includes(name) || ships.includes(name);

/**
 * What a Deployment ships, for its row: "Deploys every service", or the Services a targeted Deploy named followed by the
 * others it ships by reference, "Deploys web (+ api, postgres)"; nothing for one that takes its Environment off the Servers.
 */
export function deploysLabel({ services, ships, remove }: Pick<DeploymentSummary, "services" | "ships" | "remove">) {
  if (remove) return null;
  if (services.length === 0) return "Deploys every service";
  const referenced = ships.filter((name) => !services.includes(name)).sort();
  return `Deploys ${services.join(", ")}${referenced.length > 0 ? ` (+ ${referenced.join(", ")})` : ""}`;
}

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

/**
 * Where a Deployment's upload came from: "Uploaded by nick · abc1234 + changes"; it names no uploader who is `starter`.
 * A waiting Deploy that chooses its upload again as it starts says so.
 */
export function uploadLabel(
  { upload, chooses_upload_at_start }: Pick<DeploymentSummary, "upload" | "chooses_upload_at_start">,
  starter?: string | null,
) {
  if (!upload) return null;
  const base = upload.base ? ` · ${upload.base.commit.slice(0, 7)}${upload.base.changed ? " + changes" : ""}` : "";
  const forNow = chooses_upload_at_start ? " for now; chosen again when it starts" : "";
  return `Uploaded${upload.uploader && upload.uploader !== starter ? ` by ${upload.uploader}` : ""}${base}${forNow}`;
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
    commit ? commit.slice(0, 7) : uploadLabel(deployment, by),
  ].filter(Boolean).join(" · ");
}

/**
 * Whether a failed Deployment's `node` can be fixed on a Branch: the Deployment ran and `node` didn't apply. With no
 * Server the failure is having none, and adding one is the way on.
 */
export const canFixOnBranch = (deployment: DeploymentView, node: NodeOutcome, noServers: boolean) =>
  !noServers && deployment.status === "failed" && deployment.outcome?.type === "executed" && !nodeApplied(node.outcome);

/**
 * What a Deployment shipped despite, in words: "TOKEN not set", with the variable without a value it reads, a broken
 * reference left out, where a Config file left one empty, a Server left without ingress, ingress it couldn't check, or
 * an image exposing a port its domains don't send to.
 */
export function deploymentWarningText(warning: DeploymentWarning, service?: string) {
  if ("warning" in warning) {
    const found = warning.warning;
    return found.type === "image_port_mismatch"
      ? `${service ?? found.service}'s image ${found.image} exposes ${found.exposed.join(", ")}; its domains send to ${found.routed.join(", ")}`
      : "The deployment found something to check";
  }
  if ("cause" in warning) return `Couldn't check ingress: ${warning.cause.replace(/\.$/, "")}. Deploy again to retry`;
  if ("ingress" in warning) {
    return warning.ingress.type === "port_taken"
      ? `Ingress isn't running on ${warning.server}: something else holds port 80 or 443`
      : `Ingress isn't running on ${warning.server}: ${warning.ingress.cause}`;
  }
  if ("service" in warning) {
    if (warning.broken) return `Broken reference: ${warning.variable} in ${warning.service}`;
    const notSet = `${warning.variable} not set in ${warning.service}`;
    if (!warning.reads) return notSet;
    if (!warning.sealed_elsewhere) return `${notSet}: it reads ${warning.reads}, which is not set`;
    const [owner, key] = warning.reads.split(".", 2);
    return `${notSet}: it reads ${warning.reads}, a sealed value this branch doesn't get from its parent. Make ${owner} separate here, then set ${key}`;
  }
  if ("secret" in warning) {
    const at = `${warning.config}/${warning.file}`;
    return warning.broken ? `Broken reference: ${warning.secret} in ${at}` : `${warning.secret} not set in ${at}`;
  }
  return `${warning.variable} may break ${warning.config}/${warning.file}`;
}

/**
 * The Service node a deploy warning is about. The engine names Services by their runtime (Private DNS) name, which
 * outlives a rename, so the match goes through `runtime_names` rather than the node's shown name: a Service renamed
 * since keeps its link, and another one that took its old name is never chosen.
 */
export function warnedService(deployment: Pick<DeploymentView, "nodes" | "runtime_names">, warning: DeploymentWarning) {
  if (!("warning" in warning) || warning.warning.type !== "image_port_mismatch") return undefined;
  const runtime = warning.warning.service;
  return deployment.nodes.find((node) => node.type === "service" && (deployment.runtime_names[node.name] ?? node.name) === runtime);
}

/** Why a Deployment ended: its reason and deepest cause, or what a forgotten removal left. */
export function outcomeReason(outcome: Outcome | null) {
  if (outcome?.type === "forgotten") return "No server was enrolled any more, so whatever ran on the old ones is still there.";
  if (!outcome || !("reason" in outcome)) return undefined;
  // Work the Deployment's cancel stopped: the reason would only repeat its Cancelled status.
  if (outcome.type === "executed" && outcome.cancelled) return undefined;
  const reason = outcome.reason ?? undefined;
  const cause = outcome.cause?.at(-1);
  if (!reason || !cause) return reason;
  // A reason in whole sentences takes its cause as a closing one. Never split a reason: a dot inside may be an image tag's,
  // and with several Servers tried the cause is the last one's.
  if (!/[.!?]$/.test(reason.trimEnd())) return `${reason}: ${cause}`;
  return `${reason.trimEnd()} Cause: ${cause}${/[.!?]$/.test(cause) ? "" : "."}`;
}

/**
 * Why `node` has no deploy logs in the Deployment: nothing of it ran, its turn never came, or it was removed. Null when it
 * may have some.
 */
export function missingDeployLogs(deployment: DeploymentView, node: NodeOutcome) {
  if (deployment.outcome?.type === "never_ran" || deployment.outcome?.type === "forgotten") return "Nothing ran on a server";
  if (deployment.started_at === null || deployment.outcome?.type === "not_executed") return "Not started";
  return node.outcome === "not_attempted" || node.outcome === "removed" ? nodeStatusLabels[node.outcome] : null;
}

/** The one action a Deployment page shows by itself. */
export type DeploymentAction = "add_server" | "redeploy" | "start" | "cancel";

/**
 * Redeploy an ended Deployment, start a queued one, cancel one before it ends; with no Server, adding one comes first.
 * One waiting for CI only cancels: passing CI queues it. Cancelled while it waited, it froze nothing to redeploy.
 */
function primaryAction(status: DeploymentStatus, held: boolean, noServers: boolean): DeploymentAction | null {
  const redeploy = status === "applied" || status === "failed" || status === "unknown" || (status === "cancelled" && !held);
  if (noServers && (redeploy || status === "queued")) return "add_server";
  if (redeploy) return "redeploy";
  if (status === "queued") return "start";
  return status === "running" || status === "waiting_for_ci" ? "cancel" : null;
}

/**
 * The confirm Redeploy opens when it rolls back Deployments that ran after this one; null when it rolls nothing back
 * and redeploys in one click.
 */
export function rollbackConfirm(deployment: Pick<DeploymentView, "number" | "rolls_back">) {
  const undone = deployment.rolls_back.map((number) => `#${number}`);
  if (undone.length === 0) return null;
  const list = undone.length === 1 ? undone[0] : `${undone.slice(0, -1).join(", ")} and ${undone.at(-1)}`;
  return { title: `Roll back to #${deployment.number}`, body: `Undoes ${list}` };
}

/**
 * What a redeploy rolls back now, from the Store's refusal when it changed after the person confirmed; null for any
 * other refusal.
 */
export function freshRollback(refusal: { code: string; details: unknown }): readonly number[] | null {
  if (refusal.code !== "confirmation_required") return null;
  return Option.getOrNull(decodeRollback(refusal.details))?.rolls_back ?? null;
}

const decodeRollback = Schema.decodeUnknownOption(Schema.Struct({ rolls_back: Schema.Array(Schema.Number) }));

/**
 * The action a Deployment page shows, and whether Cancel waits in its menu beside another one. A removal that applied
 * left nothing to redeploy.
 */
export function deploymentActions(deployment: Pick<DeploymentSummary, "status" | "remove" | "held">, noServers: boolean) {
  const { status } = deployment;
  const primary = deployment.remove && status === "applied" ? null : primaryAction(status, deployment.held, noServers);
  return { primary, cancelInMenu: (status === "queued" || status === "running") && primary !== "cancel" };
}
