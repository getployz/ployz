import type {
  Automated, ConfigItemView, ConfigView, ConfigWritten, DeploymentSummary, DeploymentView, DiffView, EnvironmentsView, EnvironmentView, FollowHint, JsonValue, MergeRow,
  NodeChange, OnMerge, OnMergeInto, Outcome, PullRequestView, Removal, ServerRow, ServiceSettingChange, ServiceView, SettingRow, SyncRow,
} from "@ployz/sdk";
import type { ApprovalReview } from "#/modules/approvals/approvals";
import type { StoreAnswer, StoreCall, StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import type { SupportShows } from "./support-access";

/** What a Store cell holds, as far as support's view of it goes. */
type Holds = "variable" | "file" | "other";

const VARIABLE_PATH = /(^|\.)(env|variables)\.[^.]+$/;
const FILE_PATH = /^configs\.@[^.]+\.files\./;
/** A variable's `.exported` and `.description` share its row but aren't its value. */
const VARIABLE_META_PATH = /(^|\.)env\.[^.]+\.(exported|description)$/;

/** A row's `<lineage>:<at>` names what it holds; rows without one (renames, mounts, flags) fall back to the path. */
export function holds(row: string | null | undefined, path = ""): Holds {
  if (VARIABLE_META_PATH.test(path)) return "other";
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

/** A failed health check keeps its exit code; what the check printed is the Container's own output, shown only with its logs. */
export const withoutCheckOutput = (cause: string): string => cause.replace(/(last check exited -?\d+): .*$/s, "$1");

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
  const server = (row: ServerRow): ServerRow =>
    row.state === "failed" && !shows.logs ? { ...row, log: null, cause: row.cause.map(withoutCheckOutput) } : row;
  const outcome = (shown: Outcome | null): Outcome | null =>
    shown && "cause" in shown && !shows.logs ? { ...shown, cause: shown.cause.map(withoutCheckOutput) } : shown;
  const summary = <T extends DeploymentSummary>(view: T): T => ({ ...view, outcome: outcome(view.outcome) });
  const maybeSummary = (view: DeploymentSummary | null): DeploymentSummary | null => view && summary(view);
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
      environments: view.environments.map((environment) =>
        ({ ...environment, deployment: maybeSummary(environment.deployment), destinations: environment.destinations.map(destination) })),
      // The check's summary is markdown with values in it; Cloud publishes it to GitHub, the dashboard never shows it.
      check: view.check && !shows.variables ? { ...view.check, summary: "" } : view.check,
    }),
    config: <T extends ConfigItemView>(view: T): T => shows.configs ? view : { ...view, contents: {} },
    summary,
    environments: <T extends EnvironmentsView>(view: T): T =>
      ({ ...view, environments: view.environments.map((listing) => ({ ...listing, removal: maybeSummary(listing.removal) })) }),
    removal: <T extends Removal>(view: T): T =>
      view.removal === "removing" ? { ...view, left: view.left.map((left) => ({ ...left, deployment: maybeSummary(left.deployment) })) } : view,
    automated: <T extends Automated>(view: T): T =>
      ({ ...view, admitted: view.admitted.map((admitted) => ({ ...admitted, deployment: summary(admitted.deployment) })) }),
    deployment: <T extends DeploymentView>(view: T): T =>
      ({ ...summary(view), nodes: view.nodes.map((node) => ({ ...node, rows: node.rows.map(server) })) }),
  };
}

type ViewKind = ConfigView["view"];
type ViewOf<K extends ViewKind> = Extract<ConfigView, { view: K }>;

type ViewRedactions = { [K in ViewKind]: (view: ViewOf<K>) => ViewOf<K> };

/** Every view by name, so a new one can't reach support unclassified. A redactor never reads `view`, so an untagged view goes through it too. */
function viewRedactions(shows: SupportShows): ViewRedactions {
  const redact = redactions(shows);
  const same = <T>(view: T): T => view;
  return {
    environment: redact.environment,
    diff: redact.diff,
    history: (view) => ({ ...view, revisions: view.revisions.map(redact.changes) }),
    history_preview: redact.changes,
    plan: redact.changes,
    deployments: (view) => ({ ...view, deployments: view.deployments.map(redact.summary) }),
    deployment: redact.deployment,
    numbered_deployment: redact.deployment,
    service: redact.service,
    config: redact.config,
    sync: redact.sync,
    pull_request: redact.pullRequest,
    on_merge: redact.onMerge,
    build_log: same,
    services: same,
    namespace: same,
    namespaces: same,
    domains: same,
    domain: same,
    volumes: same,
    volume: same,
    removals: same,
    removed: same,
    configs: same,
    branch: same,
    branch_plan: same,
    build_order: same,
    environments: redact.environments,
    projects: same,
    due: same,
    consented: same,
    pr_plans: same,
  };
}

const redactView = <K extends ViewKind>(views: ViewRedactions, kind: K, view: ViewOf<K>): ViewOf<K> => views[kind](view);

/** `view` as support may read it. */
export function viewForSupport<V extends ConfigView>(view: V, shows: SupportShows): V {
  // SAFETY: each redactor answers the shape it was given.
  return redactView<ViewKind>(viewRedactions(shows), view.view, view) as V;
}

/** What a write wrote, as support may read it. Most writes answer with names and paths only. */
export function writtenForSupport(written: ConfigWritten, shows: SupportShows): ConfigWritten {
  switch (written.written) {
    case "on_merge": return redactions(shows).onMerge(written);
    case "deployment": return redactions(shows).summary(written);
    case "default_environment":
    case "branch_setup": return redactions(shows).environments(written);
    case "removal": return redactions(shows).removal(written);
    case "automated": return redactions(shows).automated(written);
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
    case "domain_added":
    case "domain":
    case "branch":
    case "build_order":
    case "synced":
    case "undone":
    case "removed":
    case "taken":
    case "never_synced":
    case "removal_cancelled":
    case "pr_plans":
      return written;
  }
}

/** The Store embeds these views in a refusal's details without their `view` tag. */
const UNTAGGED_REFUSAL_VIEWS = new Map<string, ViewKind>([["diff", "diff"], ["history_preview", "history_preview"]]);

const isViewKind = (kind: JsonValue | undefined, views: ViewRedactions): kind is ViewKind => typeof kind === "string" && Object.hasOwn(views, kind);

/** A refusal as support may read it: each view its details carry (a stale publish's diff, a restore's preview) goes through that view's redactor. */
export function refusalForSupport(refusal: StoreRefusal, shows: SupportShows): StoreRefusal {
  const { details } = refusal;
  if (!isObject(details)) return refusal;
  const views = viewRedactions(shows);
  const shown = Object.fromEntries(Object.entries(details).map(([key, value]) => {
    if (!isObject(value)) return [key, value];
    const kind = isViewKind(value["view"], views) ? value["view"] : UNTAGGED_REFUSAL_VIEWS.get(key);
    if (kind === undefined) return [key, value];
    // SAFETY: the value is the Store's view of that kind, tagged or under the key the Store embeds it as; views are plain JSON.
    return [key, redactView<ViewKind>(views, kind, value as ConfigView)];
  }));
  return { ...refusal, details: shown };
}

/** An approval as support may read it: the review it asks about, under the same rules. */
export const approvalForSupport = <T extends { review: ApprovalReview }>(approval: T, shows: SupportShows): T =>
  "diff" in approval.review ? { ...approval, review: { ...approval.review, diff: redactions(shows).diff(approval.review.diff) } } : approval;

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
