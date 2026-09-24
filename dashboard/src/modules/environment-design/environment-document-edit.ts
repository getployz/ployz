import { toast } from "sonner";
import { createOptimisticAction } from "@tanstack/react-db";
import { getEnvironmentsCollection } from "#/collections/collections";
import { observeFailure, type Persistable } from "#/collections/query-collection";
import { cachedByCollectionScope, type CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { environment as schemaEnvironment } from "#/modules/project/tables";
import type { SavedEnvironmentIntent } from "./saved-intent";

type SavedEnvironment = typeof schemaEnvironment.$inferSelect;

/** A save against the environment's current revision, run in queue order. */
export type EnvironmentDocumentSave = {
  environmentId: string;
  /** Saves against `revision` and returns the saved environment. */
  save: (revision: string) => Promise<{ data: SavedEnvironment }>;
  /** Toasted when saving fails. */
  failureMessage: string;
  /** Runs after the saved environment is committed, before the save counts as persisted. */
  afterSave?: () => Promise<void>;
};

export type EnvironmentDocumentEdit = EnvironmentDocumentSave & {
  /**
   * Applied to the in-memory document immediately; rolled back if saving fails.
   * An edit that changes nothing in memory (rotating a secret, say) still saves.
   * Skip silently when the target is gone: the save is still sent, and the saved document
   * (or the server's rejection toast) reconciles.
   */
  apply: (intent: SavedEnvironmentIntent) => void;
};

/**
 * The one way to change an environment document: apply now, save in the background,
 * roll back and toast on failure. Callers do not wait.
 *
 * The server accepts only the current revision, so saves to one environment run in
 * order, each against the latest revision the queue has seen. Optimistic snapshots
 * keep the revision they were taken with, so the collection cannot supply it.
 */
const getEnvironmentDocumentEditor = cachedByCollectionScope((organizationSlug, scope) => {
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  const tails = new Map<string, Promise<string>>();
  const inFlight = new Map<string, Set<Promise<unknown>>>();
  // Edits still preparing (hashing a variable value, say) have not picked an environment's queue yet.
  const preparing = new Set<Promise<unknown>>();

  async function preparationsSettled() {
    while (preparing.size) await Promise.allSettled(preparing);
  }

  async function runInQueue({ environmentId, save, failureMessage, afterSave }: EnvironmentDocumentSave, revision: string) {
    const base = tails.get(environmentId) ?? Promise.resolve(revision);
    const result = (async () => save(await base))();
    // The server's revision moves as soon as the save returns, even if local follow-up work fails;
    // a failed save leaves it on the revision it started from.
    const tail = result.then((saved) => saved.data.revision, () => base);
    tails.set(environmentId, tail);
    const saved = (async () => {
      const { data } = await result;
      await environments.writeCommitted(data);
      await afterSave?.();
    })();
    void tail.then(() => { if (tails.get(environmentId) === tail) tails.delete(environmentId); });
    try {
      await saved;
    } catch (error) {
      toast.error(error instanceof Error ? error.message : failureMessage);
      throw error;
    }
  }

  /** Resolves once every edit (including ones still preparing) has finished and its optimistic overlay is gone. */
  async function settled(environmentId: string) {
    await preparationsSettled();
    for (let set = inFlight.get(environmentId); set?.size; set = inFlight.get(environmentId)) {
      await Promise.allSettled(set);
    }
  }

  function track(environmentId: string, persisted: Promise<unknown>) {
    const set = inFlight.get(environmentId) ?? new Set();
    inFlight.set(environmentId, set);
    set.add(persisted);
    void persisted.catch(() => {}).finally(() => set.delete(persisted));
  }

  const action = createOptimisticAction<EnvironmentDocumentEdit & { revision: string }>({
    onMutate: ({ environmentId, apply }) => {
      environments.update(environmentId, (draft) => apply(draft.intent));
    },
    mutationFn: ({ revision, ...edit }) => runInQueue(edit, revision),
  });

  /** A document that is not loaded fails like a save: toasted, and observed. */
  function notLoaded(): Persistable {
    const error = new Error("Environment is not loaded.");
    toast.error(error.message);
    return observeFailure({ isPersisted: { promise: Promise.reject(error) } });
  }

  function queued(save: EnvironmentDocumentSave, revision: string): Persistable {
    const persisted = runInQueue(save, revision);
    track(save.environmentId, persisted);
    return observeFailure({ isPersisted: { promise: persisted } });
  }

  return {
    edit(edit: EnvironmentDocumentEdit): Persistable {
      const document = environments.get(edit.environmentId);
      if (!document) return notLoaded();
      const transaction = observeFailure(action({ ...edit, revision: document.revision }));
      // TanStack DB completes a transaction with no changes without saving it, so queue the save directly.
      if (transaction.mutations.length === 0) return queued(edit, document.revision);
      track(edit.environmentId, transaction.isPersisted.promise);
      return transaction;
    },
    /**
     * A save with nothing to apply up front, for commands like discard. It waits for every
     * pending edit (preparing or saving) to finish, so it runs last against the settled revision.
     */
    enqueue(save: EnvironmentDocumentSave): Persistable {
      const persisted = (async () => {
        await settled(save.environmentId);
        const revision = environments.get(save.environmentId)?.revision;
        await (revision ? queued(save, revision) : notLoaded()).isPersisted.promise;
      })();
      return observeFailure({ isPersisted: { promise: persisted } });
    },
    /** Registers an edit's preparation so `settled` and `enqueue` wait for it to join the queue. */
    preparing<T>(preparation: Promise<T>): Promise<T> {
      preparing.add(preparation);
      void preparation.catch(() => {}).finally(() => preparing.delete(preparation));
      return preparation;
    },
    settled,
  };
});

export function editEnvironmentDocument(organizationSlug: string, scope: CollectionScope, edit: EnvironmentDocumentEdit) {
  return getEnvironmentDocumentEditor(organizationSlug, scope).edit(edit);
}

/**
 * For edits that compute something asynchronously first (hashing a variable value, say).
 * A failure before the save is queued is toasted here; a failed save is toasted by the queue.
 */
export function editEnvironmentDocumentAfter(organizationSlug: string, scope: CollectionScope,
  prepare: () => Promise<EnvironmentDocumentEdit>, failureMessage: string): Persistable {
  const editor = getEnvironmentDocumentEditor(organizationSlug, scope);
  const promise = (async () => {
    let edit: EnvironmentDocumentEdit;
    try {
      edit = await editor.preparing(prepare());
    } catch (error) {
      toast.error(error instanceof Error ? error.message : failureMessage);
      throw error;
    }
    await editor.edit(edit).isPersisted.promise;
  })();
  return observeFailure({ isPersisted: { promise } });
}

export function useEnvironmentDocumentEditor(organizationSlug: string) {
  const editor = getEnvironmentDocumentEditor(organizationSlug, useCollectionScope());
  return (edit: EnvironmentDocumentEdit) => editor.edit(edit);
}

/** For commands that must run in the save queue (discard) or wait for it (publish). */
export function useEnvironmentDocumentQueue(organizationSlug: string) {
  const editor = getEnvironmentDocumentEditor(organizationSlug, useCollectionScope());
  return {
    enqueue: (save: EnvironmentDocumentSave) => editor.enqueue(save),
    settled: (environmentId: string) => editor.settled(environmentId),
  };
}
