import type {
  ConfigItemView, ConfigView, ConfigWritten, DeploymentView, DiffView, EnvironmentView, FollowHint, JsonValue, MergeRow, NodeChange,
  OnMerge, OnMergeInto, PullRequestView, ServerRow, ServiceSettingChange, ServiceView, SettingRow, SyncRow,
} from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import type { StoreAnswer, StoreCall, StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { SupportShows } from "./support-access";

/** What a Store cell holds, as far as support's view of it goes. */
type Holds = "variable" | "file" | "other";

const VARIABLE_PATH = /(^|\.)env\.[^.]+$/;
const FILE_PATH = /^configs\.@[^.]+\.files\./;

/** A row's `<lineage>:<at>` names what it holds; rows without one (renames, mounts, flags) fall back to the path. */
export function holds(row: string | null | undefined, path = ""): Holds {
  if (row) {
    const at = row.slice(row.indexOf(":") + 1);
    return at.startsWith("variables.") ? "variable" : at.startsWith("files.") ? "file" : "other";
  }
  return VARIABLE_PATH.test(path) ? "variable" : FILE_PATH.test(path) ? "file" : "other";
}

const isObject = (value: JsonValue): value is { [key: string]: JsonValue } =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** A variable's value as a sealed one reads: unset stays unset. */
const hideValue = (value: JsonValue): JsonValue =>
  value === null || (isObject(value) && "secret" in value) ? value : { secret: true };

/** A Config file's cell without its text: its mode and owner stay. */
const hideFile = (value: JsonValue): JsonValue => {
  if (!isObject(value) || !("content" in value)) return value;
  const { content: _content, ...rest } = value;
  return rest;
};

/** Support's view of each Store answer, under one Organization's `shows`. Members never pass through here. */
function redactions(shows: SupportShows) {
  const cell = (value: JsonValue, kind: Holds): JsonValue =>
    kind === "variable" && !shows.variables ? hideValue(value) : kind === "file" && !shows.configs ? hideFile(value) : value;
  const setting = (row: SettingRow): SettingRow => {
    const kind = holds(row.row, row.path);
    return { ...row, value: cell(row.value, kind), default: cell(row.default, kind) };
  };
  const change = (row: ServiceSettingChange): ServiceSettingChange => {
    const kind = holds(row.row, row.path);
    return { ...row, before: cell(row.before, kind), after: cell(row.after, kind) };
  };
  const node = (row: NodeChange): NodeChange => ({ ...row, settings: row.settings.map(change) });
  /** A Service's `values`: `env` maps each key to its value, or to `{ exported, value }`. */
  const values = <V extends { [key: string]: JsonValue }>(all: V): V => {
    const env = all["env"] ?? null;
    if (shows.variables || !isObject(env)) return all;
    const hidden = Object.fromEntries(Object.entries(env).map(([key, entry]) =>
      [key, isObject(entry) && "exported" in entry ? { ...entry, value: hideValue(entry["value"] ?? null) } : hideValue(entry)]));
    return { ...all, env: hidden };
  };
  const hint = (row: FollowHint): FollowHint => ({ ...row, value: cell(row.value, holds(row.row)) });
  const sync = (row: SyncRow): SyncRow => ({ ...row, from: cell(row.from, holds(row.row)), into: cell(row.into, holds(row.row)) });
  const merge = (row: MergeRow): MergeRow => {
    const kind = holds(row.row);
    return { ...row, production: cell(row.production, kind), lands: cell(row.lands, kind), preview: cell(row.preview, kind) };
  };
  const destination = (into: OnMergeInto): OnMergeInto => ({ ...into, rows: into.rows.map(merge) });
  const server = (row: ServerRow): ServerRow => row.state === "failed" && !shows.logs ? { ...row, log: null } : row;
  return {
    environment: <T extends EnvironmentView>(view: T): T =>
      ({ ...view, settings: view.settings.map(setting), values: view.values ? values(view.values) : view.values }),
    diff: <T extends DiffView>(view: T): T => {
      const shown = { ...view, changes: view.changes.map(node), follow_hints: view.follow_hints.map(hint) };
      if (view.draft_changes) shown.draft_changes = view.draft_changes.map(node);
      return shown;
    },
    changes: <T extends { changes: NodeChange[] }>(view: T): T => ({ ...view, changes: view.changes.map(node) }),
    service: <T extends ServiceView>(view: T): T => ({ ...view, values: values(view.values), changes: view.changes.map(change) }),
    sync: <T extends { rows: SyncRow[] }>(view: T): T => ({ ...view, rows: view.rows.map(sync) }),
    onMerge: <T extends OnMerge>(view: T): T => ({ ...view, destinations: view.destinations.map(destination) }),
    pullRequest: <T extends PullRequestView>(view: T): T => ({
      ...view,
      environments: view.environments.map((environment) => ({ ...environment, destinations: environment.destinations.map(destination) })),
      // The check's summary is markdown with values in it; Cloud publishes it to GitHub, the dashboard never shows it.
      check: view.check && !shows.variables ? { ...view.check, summary: "" } : view.check,
    }),
    config: <T extends ConfigItemView>(view: T): T => shows.configs ? view : { ...view, contents: {} },
    deployment: <T extends DeploymentView>(view: T): T =>
      ({ ...view, nodes: view.nodes.map((outcome) => ({ ...outcome, rows: outcome.rows.map(server) })) }),
  };
}

/** `view` as support may read it. Every view is named, so a new one can't reach support unclassified. */
export function viewForSupport(view: ConfigView, shows: SupportShows): ConfigView {
  const redact = redactions(shows);
  switch (view.view) {
    case "environment": return redact.environment(view);
    case "diff": return redact.diff(view);
    case "history": return { ...view, revisions: view.revisions.map(redact.changes) };
    case "history_preview":
    case "plan": return redact.changes(view);
    case "deployment":
    case "numbered_deployment": return redact.deployment(view);
    case "service": return redact.service(view);
    case "config": return redact.config(view);
    case "sync": return redact.sync(view);
    case "pull_request": return redact.pullRequest(view);
    case "on_merge": return redact.onMerge(view);
    case "deployments":
    case "build_log":
    case "services":
    case "namespace":
    case "namespaces":
    case "domains":
    case "domain":
    case "volumes":
    case "volume":
    case "removals":
    case "configs":
    case "branch":
    case "branch_plan":
    case "build_order":
    case "environments":
    case "projects":
    case "due":
    case "pr_plans":
      return view;
  }
}

/** What a write wrote, as support may read it. Most writes answer with names and paths only. */
export function writtenForSupport(written: ConfigWritten, shows: SupportShows): ConfigWritten {
  switch (written.written) {
    case "on_merge": return redactions(shows).onMerge(written);
    case "batch": return { ...written, results: written.results.map((result) => writtenForSupport(result, shows)) };
    case "project":
    case "project_renamed":
    case "environment":
    case "service":
    case "service_renamed":
    case "service_removed":
    case "volume":
    case "volume_removed":
    case "volume_renamed":
    case "config":
    case "config_removed":
    case "config_renamed":
    case "edited":
    case "published":
    case "discarded":
    case "history_staged":
    case "deployment":
    case "domain_added":
    case "domain":
    case "automated":
    case "branch":
    case "build_order":
    case "synced":
    case "undone":
    case "removed":
    case "taken":
    case "never_synced":
    case "default_environment":
    case "branch_setup":
    case "removal":
    case "removal_cancelled":
    case "pr_plans":
      return written;
  }
}

/** The review a refusal carries (asking for approval, or a stale publish), as support may read it. */
export function refusalForSupport(refusal: StoreRefusal, shows: SupportShows): StoreRefusal {
  const { details } = refusal;
  if (!isObject(details) || !isObject(details["diff"] ?? null)) return refusal;
  // SAFETY: a refusal's `diff` is the Store's DiffView of the Environment it refused.
  const diff = redactions(shows).diff(details["diff"] as DiffView);
  // SAFETY: a DiffView is plain JSON, as the Store sent it.
  return { ...refusal, details: { ...details, diff: diff as JsonValue } };
}

/** An approval as support may read it: the review it asks about, under the same rules. */
export const approvalForSupport = <T extends { review: ApprovalReview }>(approval: T, shows: SupportShows): T =>
  "diff" in approval.review ? { ...approval, review: { ...approval.review, diff: redactions(shows).diff(approval.review.diff) } } : approval;

export const diffForSupport = (view: DiffView, shows: SupportShows): DiffView => redactions(shows).diff(view);
export const environmentForSupport = (view: EnvironmentView, shows: SupportShows): EnvironmentView => redactions(shows).environment(view);

/** One Store call's answer, as support may read it. */
export function answerForSupport<C extends StoreCall>(call: C, result: StoreResult<StoreAnswer<C>>, shows: SupportShows): StoreResult<StoreAnswer<C>> {
  if (!result.ok) return { ok: false, refusal: refusalForSupport(result.refusal, shows) };
  // SAFETY: a read answers its query's view, a write what it wrote; each keeps its redact.
  const value = call.operation === "read"
    ? viewForSupport(result.value as ConfigView, shows)
    : writtenForSupport(result.value as ConfigWritten, shows);
  // SAFETY: each view keeps its own shape, so the answer is still the call's.
  return { ok: true, value: value as StoreAnswer<C> };
}
