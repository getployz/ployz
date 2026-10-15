import { useMutationState } from "@tanstack/react-query";
import type { BatchCommand, Change, ConfigCommand, ConfigView, DiscardTarget, EnvironmentRef, EnvironmentView } from "@ployz/sdk";
import { Schema } from "effect";

/** The mutation key of an Environment's pending writes; `store-write.ts` files each with its command. */
export const storeWriteKey = (organizationSlug: string, key: string) => ["store-write", organizationSlug, key] as const;

type NodeKind = "service" | "volume" | "config" | "domain";
type Scope = { path: string | null; target: DiscardTarget } | { node: NodeKind; name: string };
export type Busy = "saving" | "restoring" | "syncing" | "cancelling" | "discarding";

/** What a pending command says outright about the views it moves: a closed vocabulary, never a Store row. */
export type Mark =
  | { mark: "value"; environment: EnvironmentRef; path: string; value: Extract<Change, { op: "set" }>["value"] }
  | { mark: "leaving"; environment: EnvironmentRef; scope: Scope }
  | { mark: "arriving"; environment: EnvironmentRef; node: NodeKind; id: string; name: string }
  | { mark: "renamed"; environment: EnvironmentRef | null; node: NodeKind | "project"; from: string; to: string }
  | { mark: "busy"; environment: EnvironmentRef | null; why: Busy };

const changeMarks = (environment: EnvironmentRef, changes: readonly Change[]): Mark[] => changes.flatMap((change): Mark[] => {
  switch (change.op) {
    case "set": return [{ mark: "value", environment, path: change.path, value: change.value }];
    case "unset":
    case "rename":
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
    case "rename_project": return [{ mark: "renamed", environment: null, node: "project", from: command.project, to: command.name }];
    case "publish": return [{ mark: "busy", environment: command.environment, why: "saving" }];
    case "stage_history": return [{ mark: "busy", environment: command.environment, why: "restoring" }];
    case "cancel": return [{ mark: "busy", environment: null, why: "cancelling" }];
    case "sync":
    case "undo_sync":
    case "take": return [{ mark: "busy", environment: null, why: "syncing" }];
    case "batch": return command.commands.flatMap(marksOf);
    // A domain, a mount or a Config file carries no id the Store gave it; its row arrives with the answer.
    case "add_domain":
    case "attach_config":
    case "detach_config":
    case "put_config_file":
    case "remove_config_file":
    case "create_project":
    case "create_environment":
    case "set_volume_storage":
    case "set_volume_shared_writes":
    case "admit":
    case "start":
    case "set_generated_domain":
    case "create_branch":
    case "remove_proposal":
    case "copy_node":
    case "never_sync":
    case "set_merge_value":
    case "keep_branch":
    case "set_build_order":
    case "set_default_environment":
    case "set_branch_setup":
    case "remove":
    case "cancel_removal":
    case "set_pr_plan":
    case "approve_pull_request": return [];
    default: return absurd(command);
  }
}

function absurd(command: never): never {
  throw new Error(`No marks for ${JSON.stringify(command)}`);
}

/** The marks of this tab's pending writes, in submission order. */
export function usePendingMarks(organizationSlug: string): Mark[] {
  const commands = useMutationState({
    filters: { mutationKey: ["store-write", organizationSlug], status: "pending" },
    // SAFETY: every mutation under this key is `store-write.ts`'s, whose variables are `{ command }`.
    select: (mutation) => (mutation.state.variables as { command: ConfigCommand } | undefined)?.command,
  });
  return commands.flatMap((command) => command ? marksOf(command) : []);
}

const sameEnvironment = (mark: Mark, environment: EnvironmentRef) =>
  mark.environment === null || (mark.environment.project ?? "") === (environment.project ?? "") && (mark.environment.environment ?? "") === (environment.environment ?? "");

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

/** A Service the Store hasn't answered for yet: the canvas draws it as a ghost. */
export type Arriving = { pending: "arriving"; id: string; name: string };

/** Services arriving (ghosts) and leaving (dimmed) in the Environment until their writes answer. */
export function useStorePendingNodes(organizationSlug: string, environment: EnvironmentRef) {
  const marks = usePendingMarks(organizationSlug).filter((mark) => sameEnvironment(mark, environment));
  const arriving = marks.flatMap((mark): Arriving[] => mark.mark === "arriving" && mark.node === "service" ? [{ pending: "arriving", id: mark.id, name: mark.name }] : []);
  const leaving = new Set(marks.flatMap((mark) => mark.mark === "leaving" && "node" in mark.scope && mark.scope.node === "service" ? [mark.scope.name] : []));
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

/**
 * `view` with the literal edits of `marks` echoed: a set value on its own row.
 * Nothing derived: the review, counts, pink and dependents wait for the Store's rows in the write's answer.
 */
export function showPending<V extends ConfigView>(view: V, marks: readonly Mark[]): V {
  if (view.view !== "environment") return view;
  return { ...view, ...withPendingChanges(view, marks.flatMap((mark): Change[] =>
    mark.mark === "value" ? [{ op: "set", path: mark.path, value: mark.value }] : [])) };
}

const isSecret = Schema.is(Schema.Struct({ secret: Schema.Unknown }));

/**
 * Shows pending sets over an Environment view, in order, each as its value on its row (a new variable adds its row).
 * A secret shows as reads show it. Unsets and renames wait for the Store's answer.
 */
export function withPendingChanges(view: EnvironmentView, changes: readonly Change[]): EnvironmentView {
  if (changes.length === 0) return view;
  const settings = view.settings.map((row) => ({ ...row }));
  for (const change of changes) {
    if (change.op !== "set") continue;
    let row = settings.find((candidate) => candidate.path === change.path);
    if (!row) settings.push(row = { path: change.path, value: null, default: null, apply: "staged" });
    const value = change.value;
    // Keeping a secret (`{"secret": true}`) keeps what it shows, a value or none.
    row.value = !isSecret(value) ? value : value.secret === true && isSecret(row.value) ? row.value : { secret: true };
  }
  return { ...view, settings };
}
