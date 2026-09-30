import { MutationObserver } from "@tanstack/react-query";
import type { Change, ConfigCommand, ConfigWritten, EnvironmentRef } from "@ployz/sdk";
import { toast } from "sonner";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { writeStoreServerFn } from "./store.functions";
import type { StoreRefusal } from "./store.contract";
import { cachedRevision, environmentKey, refetchEnvironmentViews, storeEditKey, storeViewPrefix } from "./store-view.queries";
import { applyOptimistic } from "./store-optimistic";

/** A Store refusal thrown to a write's caller: `code` and `details` as the Store gave them. */
export class StoreRefused extends Error {
  readonly code: string;
  readonly details: unknown;
  constructor(refusal: StoreRefusal) {
    super(refusal.message);
    this.code = refusal.code;
    this.details = refusal.details;
  }
}

/** Edits to one Environment's Settings, applied at once and saved in the background. */
export type StoreEdit = {
  environment: EnvironmentRef;
  changes: Change[];
};

/** Commands that read or write an Environment besides the one they name. */
const SPANS: ReadonlySet<ConfigCommand["command"]> = new Set(["move", "create_branch", "copy_node"]);

const CONFLICT = "Changed elsewhere, so this edit was undone. You're seeing the latest now.";
/** A write that never got an answer (offline, Cloud restarting): the browser's words ("Failed to fetch") mean nothing here. */
const UNREACHABLE = "Couldn't reach Ployz Cloud, so this change was undone. Check your connection and try again.";

/**
 * The one way the dashboard writes to the Config Store.
 *
 * Every write to an Environment runs in one queue per Environment (a mutation scope), so edits, and the commands
 * that must follow them (publish, discard, deploy), commit in the order they were made. An edit expects the newest
 * revision this tab has seen: the Store refuses it with `conflict` when Working State moved somewhere this tab
 * hasn't caught up with (the CLI, another tab), and the edit is undone rather than applied over changes nobody saw.
 */
const getStoreWriter = cachedByCollectionScope((organizationSlug, scope) => {
  const { queryClient } = scope;
  // The revision each Environment's own last save produced, which views may not show yet.
  const committed = new Map<string, number>();
  // Edits not yet settled, in any Environment: a command naming several Environments waits for all of them.
  const unsettled = new Set<Promise<unknown>>();

  async function send(command: ConfigCommand) {
    const result = await writeStoreServerFn({ data: { organizationSlug, command } });
    if (!result.ok) throw new StoreRefused(result.refusal);
    return result.value;
  }

  function expected(key: string) {
    const seen = cachedRevision(queryClient, organizationSlug, key);
    const own = committed.get(key) ?? null;
    return seen === null || (own !== null && own > seen) ? own : seen;
  }

  function trackEdit(promise: Promise<unknown>) {
    const settled = promise.then(() => undefined, () => undefined).finally(() => unsettled.delete(settled));
    unsettled.add(settled);
  }

  /**
   * Runs `work` in the Environment's queue, then waits for `refresh` so the committed state shows before the
   * pending edit's overlay goes. Failure toasts, refreshes the same views (the rollback), and rejects.
   */
  function queued<T>(key: string, mutationKey: readonly unknown[], variables: Change[], work: () => Promise<T>, refresh: () => Promise<void>,
    expects: boolean, handled: readonly string[] = []) {
    const observer = new MutationObserver<T, Error, Change[]>(queryClient, {
      mutationKey,
      scope: { id: `store:${organizationSlug}:${key}` },
      mutationFn: async () => {
        const value = await work();
        await refresh();
        return value;
      },
      onError: async (error) => {
        // Only a write that sent `expect` meets a stale revision; a command's `conflict` (a taken name) says its own.
        // `confirmation_required` is a question for the caller to put to the user, not a failure; so is any refusal
        // the caller says it handles.
        if (!(error instanceof StoreRefused && (error.code === "confirmation_required" || handled.includes(error.code)))) {
          toast.error(expects && error instanceof StoreRefused && error.code === "conflict" ? CONFLICT
            : error instanceof TypeError ? UNREACHABLE : error.message);
        }
        // What the write was refused against shows: the rollback, or a Move's fresh review.
        await refresh();
      },
    });
    return observer.mutate(variables);
  }

  return {
    edit({ environment, changes }: StoreEdit): Persistable {
      const key = environmentKey(environment);
      const promise = queued(key, storeEditKey(organizationSlug, key), changes, async () => {
        const written = await send({ command: "edit", environment, expect: expected(key), changes });
        if (written.written === "edited") committed.set(key, written.environment.revision);
        return written;
      }, () => refetchEnvironmentViews(queryClient, organizationSlug, key), true);
      trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
    /**
     * Any other command: shown at once in the cached views (`applyOptimistic`), saved in the background. One naming an
     * Environment runs after that Environment's pending edits; one that reads or writes another Environment too (a
     * Move, a new Branch, a copied node: often the implied Parent) runs after every pending edit. It persists once every
     * Store view of the Organization has refetched, which also replaces the guess. A refusal toasts here, unless its code
     * is one the caller `handles`, rolls the views back, and rejects with `StoreRefused`. UI that awaits it (a Deploy,
     * a destructive confirmation, an external service) is a listed command in the boundary test.
     */
    commit(command: ConfigCommand, handles: readonly string[] = []): { isPersisted: { promise: Promise<ConfigWritten> } } {
      const key = "environment" in command && command.environment ? environmentKey(command.environment) : "";
      applyOptimistic(queryClient, organizationSlug, command);
      // A command that edits Working State expects the newest revision, as an edit does, and is tracked like one.
      const expects = "expect" in command;
      const save = async () => {
        const written = await send(expects ? { ...command, expect: expected(key) } : command);
        if (expects && "environment" in written && typeof written.environment.revision === "number") committed.set(key, written.environment.revision);
        return written;
      };
      // Only the edits made before it: an edit queued behind it in its own Environment must not be waited for.
      // ponytail: waits for edits in every Environment, not just the ones it touches; edits settle in a round trip.
      const preceding = SPANS.has(command.command) ? [...unsettled] : [];
      const work = async () => { await Promise.all(preceding); return save(); };
      const promise = queued(key, ["store-command", organizationSlug, key], [], work,
        () => queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug) }), expects, handles);
      if (expects) trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
  };
});

export function useStoreWriter(organizationSlug: string) {
  return getStoreWriter(organizationSlug, useCollectionScope());
}
