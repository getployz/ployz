import { MutationObserver } from "@tanstack/react-query";
import { Schema } from "effect";
import type { Change, ConfigCommand, ConfigWritten, EnvironmentRef } from "@ployz/sdk";
import { toast } from "sonner";
import { isUnauthorized } from "#/lib/error-message";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { writeStoreServerFn } from "./store.functions";
import { commandEnvironment, commandEnvironments, isProtectedAsk, StoreRefused } from "./store.contract";
import { cachedEnvironment, environmentKey, invalidateViews, putAnswers, shownQueries } from "./store-view.queries";
import { pendingMarks, renameBlocks, storeWriteKey } from "./store-pending";

/** Edits to one Environment's Settings, applied at once and saved in the background. */
export type StoreEdit = {
  environment: EnvironmentRef;
  changes: Change[];
};

/**
 * Commands whose views reach past the Environment they name, so their views are every view. Most read or write
 * another Environment, and may make or close one, which the Project's views (its Environments, the Projects) list.
 * A Project's Default Environment, Branch setup and protection show in those Project views, which name no Environment.
 */
const SPANS: ReadonlySet<ConfigCommand["command"]> = new Set([
  "sync", "undo_sync", "remove_proposal", "take", "create_branch", "copy_node", "set_default_environment", "set_branch_setup",
  "set_environment_protected",
]);

/** The Environment a write moved, at the revision it produced; a Batch's is its last command's. */
function writtenEnvironment(written: ConfigWritten): { id: string; revision: number } | null {
  const last = written.written === "batch" ? written.results.at(-1) : written;
  return last?.written === "service" || last?.written === "volume" || last?.written === "config" || last?.written === "edited" || last?.written === "discarded" || last?.written === "history_staged"
    ? last.environment : null;
}

const isStaleRevision = Schema.is(Schema.Struct({ revision: Schema.Number }));
const isStale = (error: Error) => error instanceof StoreRefused && error.code === "conflict" && isStaleRevision(error.details);

/** A write into a Protected Environment that now waits for an owner or admin to approve it. */
export const isRequested = (error: Error) => error instanceof StoreRefused && isProtectedAsk(error.refusal);

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
 *
 * A command acting on a review (`version`) names the one the user saw. Pressing Deploy leaves a field, whose draft
 * stages ahead of it in the queue, so a Deploy's (and a Save's) version is advanced through this tab's own writes
 * (`moved`): a revision made elsewhere stops it, and the Store refuses the Deploy as stale, as it would any review
 * nobody saw. A Discard, a History restore or a Shut down goes at the version it names: each acts on what its review
 * listed, and an edit ahead of it would be acted on unseen.
 */
const getStoreWriter = cachedByCollectionScope((organizationSlug, scope) => {
  const { queryClient } = scope;
  // The revision each Environment's own last save produced, which views may not show yet.
  const committed = new Map<string, number>();
  // Edits not yet settled, in any Environment: a command naming several Environments waits for all of them.
  const unsettled = new Set<Promise<boolean>>();
  // The run of revisions this tab's own writes made in each Environment (by its id, which outlives its name): from the
  // one the first expected to the one the last made. A write expecting any other revision starts the run over.
  const moved = new Map<string, { from: number; to: number }>();

  function record(id: string, from: number | null, to: number | null) {
    if (from === null || to === null || from === to) return;
    const own = moved.get(id);
    moved.set(id, own && own.to === from ? { from: own.from, to } : { from, to });
  }

  /** The Working State revision a review `version` names (its first part). */
  function reviewedRevision(version: string): number | null {
    const head = version.split(":")[0] ?? "";
    return /^\d+$/.test(head) ? Number(head) : null;
  }

  /** `version` advanced through this tab's own writes since the review it names; a revision made elsewhere stops it. */
  function advanced(key: string, version: string): string {
    const from = reviewedRevision(version);
    const environment = cachedEnvironment(queryClient, organizationSlug, key);
    const own = environment && moved.get(environment.id);
    if (from === null || !own || from < own.from || from >= own.to) return version;
    return [String(own.to), ...version.split(":").slice(1)].join(":");
  }

  /** The review `command` acts on, when it names one (a Deploy or Discard without one acts on whatever is there). */
  function versionOf(command: ConfigCommand): string | null {
    return "version" in command ? command.version ?? null : null;
  }

  function withVersion(command: ConfigCommand, version: string): ConfigCommand {
    return "version" in command ? { ...command, version } : command;
  }

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
    const seen = cachedEnvironment(queryClient, organizationSlug, key)?.revision ?? null;
    const own = committed.get(key) ?? null;
    return seen === null || (own !== null && own > seen) ? own : seen;
  }

  function trackEdit(promise: Promise<unknown>) {
    const settled = promise.then(() => true, () => false).finally(() => unsettled.delete(settled));
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
        if (isRequested(error)) {
          toast.success("Requested");
        } else if (isUnauthorized(error)) {
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
        const expect = expected(key);
        const written = await send({ command: "edit", environment, expect, changes });
        if (written.written !== "edited") return written;
        committed.set(key, written.environment.revision);
        record(written.environment.id, expect, written.environment.revision);
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
     *
     * One acting on a review (`version`) in its Environment is queued at once, behind the edits made before it and
     * ahead of any made after. A Deploy or a Save goes with that review advanced through this tab's own edits ahead
     * of it; any other goes at the version it names, and the Store refuses it once an edit has moved past that.
     */
    commit(command: ConfigCommand, handles: readonly string[] = []): { isPersisted: { promise: Promise<ConfigWritten> } } {
      const environment = commandEnvironment(command);
      const key = environment ? environmentKey(environment) : "";
      // A command that edits Working State expects the newest revision, as an edit does, and is tracked like one; a
      // Batch holding an edit is tracked too, so a command spanning Environments waits for it.
      const expects = "expect" in command;
      const edits = expects || (command.command === "batch" && command.commands.some((inner) => inner.command === "edit"));
      // Only the edits made before it: an edit queued behind it in its own Environment must not be waited for.
      const spans = SPANS.has(command.command) || commandEnvironments(command).length > 1;
      const reviewed = environment && !spans ? versionOf(command) : null;
      const follows = command.command === "publish" || (command.command === "admit" && command.admit === "deploy");
      const save = async () => {
        const expect = expects ? expected(key) : null;
        const version = reviewed !== null && follows ? advanced(key, reviewed) : reviewed;
        const written = await send(expects ? { ...command, expect } : version !== null ? withVersion(command, version) : command);
        const made = writtenEnvironment(written);
        if (made === null) return written;
        committed.set(key, made.revision);
        record(made.id, expect ?? (version === null ? null : reviewedRevision(version)), made.revision);
        return written;
      };
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
