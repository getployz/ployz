import { useMutationState, type Mutation, type MutationFilters, type QueryClient } from "@tanstack/react-query";
import type { BatchCommand, Change, ConfigCommand, ConfigView, DiscardTarget, EnvironmentRef, EnvironmentView, RowId, SetPrPlan } from "@ployz/sdk";
import { Schema } from "effect";
import { commandEnvironments } from "./store.contract";

/** The mutation key of an Environment's pending writes; `store-write.ts` files each with its command. */
export const storeWriteKey = (organizationSlug: string, key: string) => ["store-write", organizationSlug, key] as const;

export type NodeKind = "service" | "volume" | "config" | "domain";
type Scope = { path: string | null; target: DiscardTarget } | { node: NodeKind; name: string };
export type Busy = "saving" | "restoring" | "syncing" | "discarding" | "deploying";

const PLAN_FIELDS = ["enabled", "start_from", "copy", "setup", "remove_on_close", "include_bots"] as const;
type PlanField = (typeof PLAN_FIELDS)[number];

/** The fields a pending PR plan write sets, as it names them. */
export type PrPlanChange = { [K in PlanField]?: NonNullable<SetPrPlan[K]> };

/** What a pending command says outright about the views it moves: a closed vocabulary, never a Store row. */
export type Mark =
  | { mark: "value"; environment: EnvironmentRef; path: string; value: Extract<Change, { op: "set" }>["value"] }
  | { mark: "unset"; environment: EnvironmentRef; path: string }
  | { mark: "leaving"; environment: EnvironmentRef; scope: Scope }
  | { mark: "arriving"; environment: EnvironmentRef; node: NodeKind; id: string; name: string }
  | { mark: "renamed"; environment: EnvironmentRef; node: NodeKind | "variable"; from: string; to: string }
  | { mark: "project_renamed"; from: string; to: string }
  | { mark: "busy"; environment: EnvironmentRef; why: Busy }
  | { mark: "never_sync"; environment: EnvironmentRef; rows: readonly string[]; off: boolean }
  | { mark: "domain"; environment: EnvironmentRef; service: string; name: string | null }
  | { mark: "merge_value"; environment: EnvironmentRef; row: string }
  | { mark: "cancel"; deployment: string }
  | { mark: "pr_plan"; project: string | null; repository: string; change: PrPlanChange };

function setFields(command: SetPrPlan): PrPlanChange {
  return Object.fromEntries(PLAN_FIELDS.flatMap((field) => command[field] === null ? [] : [[field, command[field]]]));
}

const changeMarks = (environment: EnvironmentRef, changes: readonly Change[]): Mark[] => changes.flatMap((change): Mark[] => {
  switch (change.op) {
    case "set": return [{ mark: "value", environment, path: change.path, value: change.value }];
    case "rename": return [{ mark: "renamed", environment, node: "variable", from: change.path, to: change.to }];
    case "unset": return [{ mark: "unset", environment, path: change.path }];
    case "patch": return [];
  }
});

/** The marks of one command: exhaustive, so a new command kind must say what it shows. */
export function marksOf(command: ConfigCommand | BatchCommand): Mark[] {
  switch (command.command) {
    case "edit": return changeMarks(command.environment, command.changes);
    case "discard": return [
      { mark: "leaving", environment: command.environment, scope: { path: command.path, target: command.target ?? "head" } },
      { mark: "busy", environment: command.environment, why: "discarding" },
    ];
    case "remove_service": return [{ mark: "leaving", environment: command.environment, scope: { node: "service", name: command.service } }];
    case "remove_volume": return [{ mark: "leaving", environment: command.environment, scope: { node: "volume", name: command.volume } }];
    case "delete_config": return [{ mark: "leaving", environment: command.environment, scope: { node: "config", name: command.config } }];
    case "remove_domain": return [{ mark: "leaving", environment: command.environment, scope: { node: "domain", name: command.domain } }];
    case "create_service":
    case "create_git_service": return [{ mark: "arriving", environment: command.environment, node: "service", id: command.id, name: command.name }];
    case "create_volume": return [{ mark: "arriving", environment: command.environment, node: "volume", id: command.id, name: command.name }];
    case "create_config": return [{ mark: "arriving", environment: command.environment, node: "config", id: command.id, name: command.name }];
    case "rename_service": return [{ mark: "renamed", environment: command.environment, node: "service", from: command.service, to: command.name }];
    case "rename_volume": return [{ mark: "renamed", environment: command.environment, node: "volume", from: command.volume, to: command.name }];
    case "rename_config": return [{ mark: "renamed", environment: command.environment, node: "config", from: command.config, to: command.name }];
    case "rename_project": return [{ mark: "project_renamed", from: command.project, to: command.name }];
    case "publish": return [{ mark: "busy", environment: command.environment, why: "saving" }];
    case "stage_history": return [{ mark: "busy", environment: command.environment, why: "restoring" }];
    case "sync":
    case "take": return commandEnvironments(command).map((environment): Mark => ({ mark: "busy", environment, why: "syncing" }));
    case "batch": return command.commands.flatMap(marksOf);
    case "never_sync": return [{ mark: "never_sync", environment: command.environment, rows: command.rows, off: command.off ?? false }];
    case "add_domain": return [{ mark: "domain", environment: command.environment, service: command.service, name: command.hostname ?? command.prefix ?? null }];
    case "set_generated_domain": return [{ mark: "domain", environment: command.environment, service: command.service, name: command.domain ?? command.prefix }];
    case "set_merge_value": return [{ mark: "merge_value", environment: command.environment, row: command.row }];
    case "cancel": return [{ mark: "cancel", deployment: command.deployment }];
    case "set_pr_plan": return [{ mark: "pr_plan", project: command.project, repository: command.repository, change: setFields(command) }];
    case "admit": return command.admit === "deploy" ? [{ mark: "busy", environment: command.environment, why: "deploying" }] : [];
    // A mount or a Config file carries no id the Store gave it; its row arrives with the answer.
    case "undo_sync":
    case "attach_config":
    case "detach_config":
    case "put_config_file":
    case "remove_config_file":
    case "create_project":
    case "create_environment":
    case "set_volume_storage":
    case "set_volume_shared_writes":
    case "start":
    case "create_branch":
    case "remove_proposal":
    case "copy_node":
    case "keep_branch":
    case "set_build_order":
    case "set_default_environment":
    case "set_branch_setup":
    case "remove":
    case "cancel_removal":
    case "approve_pull_request": return [];
    default: return absurd(command);
  }
}

function absurd(command: never): never {
  throw new Error(`No marks for ${JSON.stringify(command)}`);
}

const pendingWrites = (organizationSlug: string): MutationFilters => ({ mutationKey: ["store-write", organizationSlug], status: "pending" });

// SAFETY: every mutation under this key is `store-write.ts`'s, whose variables are `{ command }`.
const commandOf = (mutation: Mutation<unknown, Error, unknown>) => (mutation.state.variables as { command: ConfigCommand } | undefined)?.command;

/** The marks of this tab's pending writes, in submission order. */
export function usePendingMarks(organizationSlug: string): Mark[] {
  return useMutationState({ filters: pendingWrites(organizationSlug), select: commandOf }).flatMap((command) => command ? marksOf(command) : []);
}

/** The same marks, read outside React: the writer checks a command against them before queueing it. */
export function pendingMarks(queryClient: QueryClient, organizationSlug: string): Mark[] {
  return queryClient.getMutationCache().findAll(pendingWrites(organizationSlug)).flatMap((mutation) => {
    const command = commandOf(mutation);
    return command ? marksOf(command) : [];
  });
}

const sameEnvironment = (mark: Mark, environment: EnvironmentRef) => "environment" in mark &&
  (mark.environment.project ?? "") === (environment.project ?? "") && (mark.environment.environment ?? "") === (environment.environment ?? "");

/** Whether `project` names the one a Project rename moves; none means the Organization's only Project, which may be it. */
const sameProject = (from: string, project: string | null | undefined) => project == null || project === from;

/** Why the Environment is mid-command, if it is: the bottom bar and review say so. */
export function useStoreBusy(organizationSlug: string, environment: EnvironmentRef): Busy | null {
  const busy = usePendingMarks(organizationSlug).find((mark) => mark.mark === "busy" && sameEnvironment(mark, environment));
  return busy?.mark === "busy" ? busy.why : null;
}

/** The paths of the Environment's pending Discards: the review dims each clicked line until the answer. */
export function useStoreDiscarding(organizationSlug: string, environment: EnvironmentRef): ReadonlySet<string> {
  return new Set(usePendingMarks(organizationSlug).flatMap((mark) =>
    mark.mark === "leaving" && sameEnvironment(mark, environment) && "path" in mark.scope && mark.scope.path !== null ? [mark.scope.path] : []));
}

/** A node the Store hasn't answered for yet: the canvas draws it as a ghost. */
export type Arriving = { pending: "arriving"; node: NodeKind; id: string; name: string };

/** Nodes arriving (ghosts) and leaving (dimmed, by kind and name) in the Environment until their writes answer. */
export function useStorePendingNodes(organizationSlug: string, environment: EnvironmentRef) {
  const marks = usePendingMarks(organizationSlug).filter((mark) => sameEnvironment(mark, environment));
  const arriving = marks.flatMap((mark): Arriving[] => mark.mark === "arriving" ? [{ pending: "arriving", node: mark.node, id: mark.id, name: mark.name }] : []);
  const leavingOf = (node: NodeKind): ReadonlySet<string> =>
    new Set(marks.flatMap((mark) => mark.mark === "leaving" && "node" in mark.scope && mark.scope.node === node ? [mark.scope.name] : []));
  const leaving = { service: leavingOf("service"), volume: leavingOf("volume"), config: leavingOf("config"), domain: leavingOf("domain") } satisfies Record<NodeKind, ReadonlySet<string>>;
  return { arriving, leaving };
}

/**
 * A node's name as shown on its title and name field: a pending rename's new name until the Store answers.
 * Only the label moves; the node's rows and dependents keep the name the Store gave them until its answer lands.
 */
export function useStorePendingName(organizationSlug: string, environment: EnvironmentRef, node: NodeKind, name: string): string {
  const renamed = usePendingMarks(organizationSlug).filter((mark) => mark.mark === "renamed" && mark.node === node && sameEnvironment(mark, environment));
  return renamed.reduce((shown, mark) => mark.mark === "renamed" && mark.from === shown ? mark.to : shown, name);
}

type VersionedCommand = { [K in ConfigCommand["command"]]: "version" extends keyof Extract<ConfigCommand, { command: K }> ? K : never }[ConfigCommand["command"]];

/** Commands that carry a version the Store checks: it refuses one a rename moved under it, so none waits on a rename. */
const VERSIONED = { publish: true, stage_history: true, discard: true, sync: true, remove_proposal: true, take: true, remove: true } satisfies Record<VersionedCommand, true>;

const renamesIn = (marks: readonly Mark[], environment: EnvironmentRef) => marks.some((mark) =>
  mark.mark === "renamed" ? sameEnvironment(mark, environment) : mark.mark === "project_renamed" && sameProject(mark.from, environment.project));

/**
 * Whether a pending rename moves something `command` may name: any node, variable or Project of an Environment it
 * writes, by a path, a mount, a reference or its Environment, or the Project it names outright. It waits for the
 * Store's answer, which names the new name.
 */
export function renameBlocks(marks: readonly Mark[], command: ConfigCommand): boolean {
  if (Object.hasOwn(VERSIONED, command.command)) return false;
  const project = "project" in command ? command.project : undefined;
  return commandEnvironments(command).some((environment) => renamesIn(marks, environment))
    || project !== undefined && marks.some((mark) => mark.mark === "project_renamed" && sameProject(mark.from, project));
}

/** Whether a rename in the Environment is pending: its canvas, panels and settings lock until the Store answers. */
export function useStoreRenamePending(organizationSlug: string, environment: EnvironmentRef): boolean {
  return renamesIn(usePendingMarks(organizationSlug), environment);
}

/** The rows pending Never syncs mark (true) or sync again (false), the newest last. */
function neverSyncing(marks: readonly Mark[]): ReadonlyMap<string, boolean> {
  return new Map(marks.flatMap((mark) => mark.mark === "never_sync" ? mark.rows.map((row) => [row, !mark.off] as const) : []));
}

/** This tab's pending Never syncs in `project`: a Sync from or into any of its Environments shows them. */
export function useStoreNeverSyncs(organizationSlug: string, project: string | null): NeverSyncing[] {
  return usePendingMarks(organizationSlug).flatMap((mark) => mark.mark === "never_sync" && mark.environment.environment !== null
    && (mark.environment.project ?? "") === (project ?? "") ? [{ environment: mark.environment.environment, rows: mark.rows, off: mark.off }] : []);
}

/** A pending Never sync: `rows` of `environment` marked, or with `off` synced again. */
export type NeverSyncing = { environment: string; rows: readonly string[]; off: boolean };

/** The names pending domain writes give `service`, none for a domain the Store names: each shows Saving until the answer. */
export function useStorePendingDomains(organizationSlug: string, environment: EnvironmentRef, service: string): (string | null)[] {
  return usePendingMarks(organizationSlug).flatMap((mark) =>
    mark.mark === "domain" && mark.service === service && sameEnvironment(mark, environment) ? [mark.name] : []);
}

/** The rows a pending On merge write or Never sync names in the Environment: each shows Saving until the answer. */
export function useStoreMergeRowsPending(organizationSlug: string, environment: EnvironmentRef): ReadonlySet<string> {
  return new Set(usePendingMarks(organizationSlug).flatMap((mark) => !sameEnvironment(mark, environment) ? []
    : mark.mark === "merge_value" ? [mark.row] : mark.mark === "never_sync" ? mark.rows : []));
}

/** Whether a Cancel of the Deployment is pending: it reads Cancelling until the Store answers. */
export function useStoreCancelling(organizationSlug: string, deployment: string): boolean {
  return usePendingMarks(organizationSlug).some((mark) => mark.mark === "cancel" && mark.deployment === deployment);
}

/** What pending writes set on a repository's PR plan, the newest last: the panel shows it until the answer. */
export function useStorePrPlanPending(organizationSlug: string, project: string | null, repository: string): PrPlanChange {
  return usePendingMarks(organizationSlug).reduce((shown: PrPlanChange, mark) => mark.mark === "pr_plan"
    && (mark.project ?? "") === (project ?? "") && mark.repository === repository ? { ...shown, ...mark.change } : shown, {});
}

/** The edit a mark echoes on an Environment view's rows, if any. */
const changeOf = (mark: Mark): Change[] => {
  switch (mark.mark) {
    case "value": return [{ op: "set", path: mark.path, value: mark.value }];
    case "unset": return [{ op: "unset", path: mark.path }];
    default: return [];
  }
};

/**
 * `view` with the literal edits of `marks` echoed: a set value on its own row, an unset row at its default, and a row
 * marked Never sync or synced again. Nothing derived: a rename, the review, counts, pink, references and dependents
 * wait for the Store's rows in the write's answer.
 */
export function showPending<V extends ConfigView>(view: V, marks: readonly Mark[]): V {
  if (view.view !== "environment") return view;
  const shown = withPendingChanges(view, marks.flatMap(changeOf));
  const marking = neverSyncing(marks);
  if (marking.size === 0) return { ...view, ...shown };
  const kept = (view.never_synced ?? []).filter((row) => marking.get(row) !== false);
  // SAFETY: the dashboard names a row only by the RowId a read gave.
  const added = [...marking].flatMap(([row, marked]) => marked && !kept.includes(row as RowId) ? [row as RowId] : []);
  return { ...view, ...shown, never_synced: [...kept, ...added] };
}

const isSecret = Schema.is(Schema.Struct({ secret: Schema.Unknown }));

/**
 * Shows pending edits over an Environment view, in order: a set as its value on its row (a new variable adds its row),
 * an unset as the row's default. A secret shows as reads show it. Renames and patches wait for the Store's answer.
 */
export function withPendingChanges(view: EnvironmentView, changes: readonly Change[]): EnvironmentView {
  if (changes.length === 0) return view;
  const settings = view.settings.map((row) => ({ ...row }));
  for (const change of changes) {
    if (change.op === "patch" || change.op === "rename") continue;
    let row = settings.find((candidate) => candidate.path === change.path);
    if (!row && change.op === "set") settings.push(row = { path: change.path, value: null, default: null, apply: "staged" });
    if (!row) continue;
    const value = change.op === "set" ? change.value : row.default;
    // Keeping a secret (`{"secret": true}`) keeps what it shows, a value or none.
    row.value = !isSecret(value) ? value : value.secret === true && isSecret(row.value) ? row.value : { secret: true };
  }
  return { ...view, settings };
}
