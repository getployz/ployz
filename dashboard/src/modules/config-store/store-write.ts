import { MutationObserver } from "@tanstack/react-query";
import { Schema } from "effect";
import type { Change, ConfigCommand, ConfigWritten, EnvironmentRef } from "@ployz/sdk";
import { toast } from "sonner";
import { isUnauthorized } from "#/lib/error-message";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { writeStoreServerFn } from "./store.functions";
import { commandEnvironment, commandEnvironments, StoreRefused } from "./store.contract";
import { cachedRevision, environmentKey, invalidateViews, putAnswers, shownQueries } from "./store-view.queries";
import { pendingMarks, renameBlocks, storeWriteKey } from "./store-pending";

/** Edits to one Environment's Settings, applied at once and saved in the background. */
export type StoreEdit = {
  environment: EnvironmentRef;
  changes: Change[];
};

/**
 * Commands whose views reach past the Environment they name, so their views are every view. Most read or write
 * another Environment, and may make or close one, which the Project's views (its Environments, the Projects) list.
 * A Project's Default Environment and Branch setup show in those Project views, which name no Environment.
 */
const SPANS: ReadonlySet<ConfigCommand["command"]> = new Set([
  "sync", "undo_sync", "remove_proposal", "take", "create_branch", "copy_node", "set_default_environment", "set_branch_setup",
]);

/** The Environment revision a write produced; a Batch's is its last command's. */
function writtenRevision(written: ConfigWritten): number | null {
  const last = written.written === "batch" ? written.results.at(-1) : written;
  return last?.written === "service" || last?.written === "volume" || last?.written === "config" || last?.written === "edited" || last?.written === "discarded" || last?.written === "history_staged"
    ? last.environment.revision : null;
}

const isStaleRevision = Schema.is(Schema.Struct({ revision: Schema.Number }));
const isStale = (error: Error) => error instanceof StoreRefused && error.code === "conflict" && isStaleRevision(error.details);

const CONFLICT = "Changed elsewhere, so this edit was undone. You're seeing the latest now.";
/** A write that never got an answer (offline, Cloud restarting): the browser's words ("Failed to fetch") mean nothing here. */
const UNREACHABLE = "Couldn't reach Ployz Cloud, so this change was undone. Check your connection and try again.";
const RENAMING = "A rename is still saving, so this change wasn't made. Try again once it lands.";

/** Writes whose control shows a refusal itself, beside the field, so a toast would only say it twice. */
const shownInline = new WeakSet<Promise<unknown>>();

/** Leaves `write`'s refusal to the control that made it; being signed out still toasts, with its Sign in. */
export function showRefusalInline(write: Persistable) {
  shownInline.add(write.isPersisted.promise);
}

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

  /** The Environments whose views a command moves (null, every view). */
  function keysOf(command: ConfigCommand): ReadonlySet<string> | null {
    const refs = commandEnvironments(command);
    return refs.length === 0 || SPANS.has(command.command) ? null : new Set(refs.map(environmentKey));
  }

  /** Sends `command` with the views this tab shows of its Environments, and installs their answers. */
  async function send(command: ConfigCommand) {
    const keys = keysOf(command);
    const shows = shownQueries(queryClient, organizationSlug, keys);
    const result = await writeStoreServerFn({ data: { organizationSlug, command, shows } });
    if (!result.ok) throw new StoreRefused(result.refusal);
    await putAnswers(queryClient, organizationSlug, keys, result.answers ?? []);
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
   * Runs `work` in the Environment's queue, filed under `storeWriteKey` with its command, whose marks show until the
   * write's answers land. Failure toasts unless its control shows it (`showRefusalInline`), rereads the command's
   * views (the rollback), and rejects. A command a pending rename blocks (`renameBlocks`) is never sent: the UI locks
   * for the rename, and this catches a control it missed.
   */
  function queued<T>(key: string, command: ConfigCommand, work: () => Promise<T>, expects: boolean, handled: readonly string[] = []) {
    if (renameBlocks(pendingMarks(queryClient, organizationSlug), command)) {
      toast.error(RENAMING);
      return Promise.reject(new StoreRefused({ code: "renaming", message: RENAMING, details: null }));
    }
    let promise: Promise<T> | undefined;
    const observer = new MutationObserver<T, Error, { command: ConfigCommand }>(queryClient, {
      mutationKey: storeWriteKey(organizationSlug, key),
      scope: { id: `store:${organizationSlug}:${key}` },
      mutationFn: work,
      onError: async (error) => {
        // Only a write that sent `expect` meets a stale revision, whose `conflict` names the revision; any other
        // `conflict` (a taken name, a referenced variable) says its own.
        // A refusal the caller says it handles (`confirmation_required`, a question it puts to the user) is its to show.
        if (isUnauthorized(error)) {
          toast.error("You're signed out. Nothing was saved.", { action: { label: "Sign in", onClick: () => window.location.assign(signInHere()) } });
        } else if (!(promise && shownInline.has(promise)) && !(error instanceof StoreRefused && handled.includes(error.code))) {
          toast.error(expects && isStale(error) ? CONFLICT : error instanceof TypeError ? UNREACHABLE : error.message);
        }
        // What the write was refused against shows: the rollback, or a Sync's fresh review.
        await invalidateViews(queryClient, organizationSlug, keysOf(command));
      },
    });
    promise = observer.mutate({ command });
    return promise;
  }

  return {
    edit({ environment, changes }: StoreEdit): Persistable {
      const key = environmentKey(environment);
      const command: ConfigCommand = { command: "edit", environment, expect: null, changes };
      const promise = queued(key, command, async () => {
        const written = await send({ command: "edit", environment, expect: expected(key), changes });
        if (written.written === "edited") committed.set(key, written.environment.revision);
        return written;
      }, true);
      trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
    /**
     * Any other command: its marks show at once over the cached views (`store-pending.ts`), and it saves in the
     * background. One naming an Environment runs after that Environment's pending edits; one that reads or writes
     * another Environment too (a Sync, a new Branch, a copied node: often the implied Parent) runs after every pending
     * edit. It persists once the write's answers replace its views. A refusal toasts here, unless its code is one the
     * caller `handles`, rereads the views, and rejects with `StoreRefused`.
     */
    commit(command: ConfigCommand, handles: readonly string[] = []): { isPersisted: { promise: Promise<ConfigWritten> } } {
      const environment = commandEnvironment(command);
      const key = environment ? environmentKey(environment) : "";
      // A command that edits Working State expects the newest revision, as an edit does, and is tracked like one; a
      // Batch holding an edit is tracked too, so a command spanning Environments waits for it.
      const expects = "expect" in command;
      const edits = expects || (command.command === "batch" && command.commands.some((inner) => inner.command === "edit"));
      const save = async () => {
        const written = await send(expects ? { ...command, expect: expected(key) } : command);
        const revision = writtenRevision(written);
        if (revision !== null) committed.set(key, revision);
        return written;
      };
      // Only the edits made before it: an edit queued behind it in its own Environment must not be waited for.
      const spans = SPANS.has(command.command) || commandEnvironments(command).length > 1;
      const preceding = spans ? [...unsettled] : [];
      const work = async () => { await Promise.all(preceding); return save(); };
      const promise = queued(key, command, work, expects, handles);
      if (edits) trackEdit(promise);
      return observeFailure({ isPersisted: { promise } });
    },
  };
});

/**
 * Writes show at once as marks of what the command says, and save in the background; the Store's answers replace them.
 * A refusal toasts and rereads the views, so a caller handles failure only for state of its own, or for a refusal
 * code it passes to `commit` as `handles` and shows itself.
 */
export function useStoreWriter(organizationSlug: string) {
  return getStoreWriter(organizationSlug, useCollectionScope());
}

/** GitHub sign-in that returns the user to the page they were on. */
function signInHere() {
  const callbackURL = window.location.pathname + window.location.search;
  return `/auth/github?${new URLSearchParams({ callbackURL })}`;
}
