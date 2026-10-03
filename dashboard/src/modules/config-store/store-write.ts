import { MutationObserver } from "@tanstack/react-query";
import type { Change, ConfigCommand, ConfigWritten, EnvironmentRef } from "@ployz/sdk";
import { toast } from "sonner";
import { isUnauthorized } from "#/lib/error-message";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { writeStoreServerFn } from "./store.functions";
import { commandEnvironment, type CommittedViews, StoreRefused } from "./store.contract";
import { cachedRevision, environmentKey, putCommittedViews, refetchAfterWrite, storeEditKey } from "./store-view.queries";
import { applyOptimistic } from "./store-optimistic";

/** Edits to one Environment's Settings, applied at once and saved in the background. */
export type StoreEdit = {
  environment: EnvironmentRef;
  changes: Change[];
};

/** The committed views a write's answer carried, filled as it answers: its refresh skips them. */
type Carried = { views?: CommittedViews };

/** Commands that read or write an Environment besides the one they name. */
const SPANS: ReadonlySet<ConfigCommand["command"]> = new Set(["sync", "undo_sync", "take", "create_branch", "copy_node"]);

/** The Environment revision a write produced; a Batch's is its last command's. */
function writtenRevision(written: ConfigWritten): number | null {
  const last = written.written === "batch" ? written.results.at(-1) : written;
  return last?.written === "service" || last?.written === "volume" || last?.written === "edited"
    ? last.environment.revision : null;
}

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

  async function send(command: ConfigCommand, carried: Carried) {
    const result = await writeStoreServerFn({ data: { organizationSlug, command } });
    if (!result.ok) throw new StoreRefused(result.refusal);
    const environment = commandEnvironment(command);
    if (result.views && environment) {
      carried.views = result.views;
      await putCommittedViews(queryClient, organizationSlug, environmentKey(environment), result.views);
    }
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
        // A refusal the caller says it handles (`confirmation_required`, a question it puts to the user) is its to show.
        if (isUnauthorized(error)) {
          toast.error("You're signed out. Nothing was saved.", { action: { label: "Sign in", onClick: () => window.location.assign("/auth") } });
        } else if (!(error instanceof StoreRefused && handled.includes(error.code))) {
          toast.error(expects && error instanceof StoreRefused && error.code === "conflict" ? CONFLICT
            : error instanceof TypeError ? UNREACHABLE : error.message);
        }
        // What the write was refused against shows: the rollback, or a Sync's fresh review.
        await refresh();
      },
    });
    return observer.mutate(variables);
  }

  return {
    edit({ environment, changes }: StoreEdit): Persistable {
      const key = environmentKey(environment);
      const carried: Carried = {};
      const promise = queued(key, storeEditKey(organizationSlug, key), changes, async () => {
        const written = await send({ command: "edit", environment, expect: expected(key), changes }, carried);
        if (written.written === "edited") committed.set(key, written.environment.revision);
        return written;
      }, () => refetchAfterWrite(queryClient, organizationSlug, key, carried.views), true);
      trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
    /**
     * Any other command: shown at once in the cached views (`applyOptimistic`), saved in the background. One naming an
     * Environment runs after that Environment's pending edits; one that reads or writes another Environment too (a
     * Sync, a new Branch, a copied node: often the implied Parent) runs after every pending edit. It persists once the
     * views it may move have refetched (its Environment's; every view for one spanning several or naming none),
     * which also replaces the guess. A refusal toasts here, unless its code
     * is one the caller `handles`, rolls the views back, and rejects with `StoreRefused`. UI that awaits it (a Deploy,
     * a destructive confirmation, an external service) is a listed command in the boundary test.
     */
    commit(command: ConfigCommand, handles: readonly string[] = []): { isPersisted: { promise: Promise<ConfigWritten> } } {
      const environment = commandEnvironment(command);
      const key = environment ? environmentKey(environment) : "";
      void applyOptimistic(queryClient, organizationSlug, command);
      // A command that edits Working State expects the newest revision, as an edit does, and is tracked like one; a
      // Batch holding an edit is tracked too, so a command spanning Environments waits for it.
      const expects = "expect" in command;
      const edits = expects || (command.command === "batch" && command.commands.some((inner) => inner.command === "edit"));
      const carried: Carried = {};
      const save = async () => {
        const written = await send(expects ? { ...command, expect: expected(key) } : command, carried);
        const revision = writtenRevision(written);
        if (revision !== null) committed.set(key, revision);
        return written;
      };
      // Only the edits made before it: an edit queued behind it in its own Environment must not be waited for.
      // ponytail: waits for edits in every Environment, not just the ones it touches; edits settle in a round trip.
      const spans = SPANS.has(command.command);
      const preceding = spans ? [...unsettled] : [];
      const work = async () => { await Promise.all(preceding); return save(); };
      const refreshed = environment === null || spans ? null : key;
      const promise = queued(key, ["store-command", organizationSlug, key], [], work,
        () => refetchAfterWrite(queryClient, organizationSlug, refreshed, carried.views), expects, handles);
      if (edits) trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
  };
});

export function useStoreWriter(organizationSlug: string) {
  return getStoreWriter(organizationSlug, useCollectionScope());
}
