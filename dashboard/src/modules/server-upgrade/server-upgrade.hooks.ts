import { useEffect, useState } from "react";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getServerUpgradeSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { type PendingFrom, type ReleaseChannel, serverUpgradeSettings } from "./server-upgrade";
import { requestServerUpgradeServerFn, setServerUpgradeSettingsServerFn } from "./server-upgrade.functions";

/** How long a click reads as Upgrading before its attempt is recorded; a Busy Server records none. */
const PENDING_MS = 60_000;

/** A click on Upgrade reads as Upgrading until a newer attempt than `newestAttemptId` is recorded, or for a minute. */
function usePendingUpgrade(newestAttemptId: string | null) {
  const [from, setFrom] = useState<PendingFrom>(undefined);
  const recorded = from !== undefined && newestAttemptId !== from;
  useEffect(() => {
    if (recorded) setFrom(undefined);
  }, [recorded]);
  useEffect(() => {
    if (from === undefined) return;
    const timer = setTimeout(() => setFrom(undefined), PENDING_MS);
    return () => clearTimeout(timer);
  }, [from]);
  return { from, start: () => setFrom(newestAttemptId), cancel: () => setFrom(undefined) };
}

/**
 * Upgrade one Server, or every Server behind when `server` is null: reads as Upgrading at once, asks Cloud in the
 * background, and toasts if Cloud refuses. `newestAttemptId` is the newest attempt the view shows.
 */
export function useRequestUpgrade(
  organizationSlug: string,
  newestAttemptId: string | null,
  server: { readonly id: string; readonly name: string } | null,
) {
  const pending = usePendingUpgrade(newestAttemptId);
  const upgrade = () => {
    pending.start();
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: server?.id ?? null } }).catch(() => {
      pending.cancel();
      toast.error(server === null ? "Could not upgrade your servers" : `Could not upgrade ${server.name}`);
    });
  };
  return { pendingFrom: pending.from, upgrade };
}

/** The clock, ticking while an attempt runs: one reads as unknown once it outlives the observation limit. */
export function useNow(running: boolean) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!running) return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [running]);
  return now;
}

type Change = { readonly automatic: boolean } | { readonly channel: ReleaseChannel };

/**
 * The Organization's Server upgrade settings: "Upgrade automatically" and its Release Channel. A change shows at once,
 * saves in the background, and rolls back with a toast.
 */
export function useServerUpgradeSettings(organizationSlug: string) {
  const collection = getServerUpgradeSettingsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const settings = serverUpgradeSettings(rows);
  const set = createOptimisticAction<Change>({
    onMutate: (change) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { Object.assign(row, change); });
      else collection.insert({ id: organizationId, ...settings, ...change });
    },
    mutationFn: (change) => setServerUpgradeSettingsServerFn({ data: { organizationSlug, ...change } })
      .then((row) => collection.writeCommitted(row)),
  });
  const save = (change: Change, failure: string) => {
    set(change).isPersisted.promise.catch(() => toast.error(failure));
  };
  return {
    ...settings,
    setAutomatic: (automatic: boolean) => save({ automatic }, "Could not change automatic upgrades"),
    setChannel: (channel: ReleaseChannel) => save({ channel }, "Could not change releases"),
  };
}
