import { MutationObserver } from "@tanstack/react-query";
import type { Change, ConfigCommand, ConfigWritten, EnvironmentRef } from "@ployz/sdk";
import { toast } from "sonner";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { writeStoreServerFn } from "./store.functions";
import type { StoreRefusal } from "./store.contract";
import { cachedRevision, environmentKey, refetchEnvironmentViews, storeEditKey, storeViewPrefix } from "./store-view.queries";

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

const CONFLICT = "Changed elsewhere, so this edit was undone. You're seeing the latest now.";

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

  /**
   * Runs `work` in the Environment's queue, then waits for `refresh` so the committed state shows before the
   * pending edit's overlay goes. Failure toasts, refreshes the same views (the rollback), and rejects.
   */
  function queued<T>(key: string, mutationKey: readonly unknown[], variables: Change[], work: () => Promise<T>, refresh: () => Promise<void>, expects: boolean) {
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
        // `confirmation_required` is a question for the caller to put to the user, not a failure.
        if (!(error instanceof StoreRefused && error.code === "confirmation_required")) {
          toast.error(expects && error instanceof StoreRefused && error.code === "conflict" ? CONFLICT : error.message);
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
      return observeFailure({ isPersisted: { promise } });
    },
    /**
     * A command whose outcome matters before the page moves on (publish, discard, deploy). One naming an Environment
     * runs after that Environment's pending edits, and it persists once every Store view of the Organization has
     * refetched. A refusal toasts here and rejects with `StoreRefused`. UI that awaits it is a listed command in
     * the boundary test; creates need not wait, because the caller mints the new id.
     */
    commit(command: ConfigCommand): { isPersisted: { promise: Promise<ConfigWritten> } } {
      const key = "environment" in command && command.environment ? environmentKey(command.environment) : "";
      const promise = queued(key, ["store-command", organizationSlug, key], [], () => send(command),
        () => queryClient.invalidateQueries({ queryKey: storeViewPrefix(organizationSlug) }), false);
      return observeFailure({ isPersisted: { promise } });
    },
  };
});

/** Edit an Environment's Settings: shown at once, saved in the background, undone and toasted on failure. */
export function editStoreEnvironment(organizationSlug: string, scope: CollectionScope, edit: StoreEdit) {
  return getStoreWriter(organizationSlug, scope).edit(edit);
}

export function useStoreWriter(organizationSlug: string) {
  return getStoreWriter(organizationSlug, useCollectionScope());
}
