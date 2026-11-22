import { refusalFor } from "#/modules/organization/permissions";
import { useEffect, useState } from "react";
import { usePendingAttempt } from "#/hooks/use-pending-attempt";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useOrganizationContext } from "#/collections/use-organization-context";
import { toast } from "sonner";
import { getServerUpgradeSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  type PendingFrom,
  type ReleaseChannel,
  serverUpgradeSettings,
} from "./server-upgrade";
import {
  requestServerUpgradeServerFn,
  setServerUpgradeSettingsServerFn,
} from "./server-upgrade.functions";

export const UPGRADE_REFUSED = refusalFor("servers:operate", "upgrade servers");

/**
 * A click on Upgrade reads as Upgrading until a newer attempt than the one it was clicked from is recorded, or for a
 * minute: a Busy Server records none.
 */
function usePendingUpgrade(newestAttemptId: string | null) {
  const [clicked, setClicked] = usePendingAttempt<{ readonly from: string | null }, string | null>(
    newestAttemptId,
    ({ from }, newest) => newest !== from,
  );
  const from: PendingFrom = clicked?.from;
  return {
    from,
    start: () => setClicked((current) => (current?.from === newestAttemptId ? current : { from: newestAttemptId })),
    cancel: () => setClicked(null),
  };
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
    requestServerUpgradeServerFn({ data: { organizationSlug, machineId: server?.id ?? null } })
      .catch(() => {
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
  const { organizationId } = useOrganizationContext();
  const settings = serverUpgradeSettings(rows);
  const set = createOptimisticAction<Change>({
    onMutate: (change) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { Object.assign(row, change); });
      else collection.insert({ id: organizationId, ...settings, ...change });
    },
    mutationFn: (change) => setServerUpgradeSettingsServerFn({ data: { organizationSlug, ...change } })
      .then((row) => collection.utils.writeUpsert(row)),
  });
  const save = (change: Change, failure: string) => {
    set(change).when("settled").catch(() => toast.error(failure));
  };
  return {
    ...settings,
    setAutomatic: (automatic: boolean) => save({ automatic }, "Could not change automatic upgrades"),
    setChannel: (channel: ReleaseChannel) => save({ channel }, "Could not change releases"),
  };
}
