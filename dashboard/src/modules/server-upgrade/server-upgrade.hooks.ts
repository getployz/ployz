import { useEffect, useState } from "react";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getServerUpgradeSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { PasskeyRefused, signWithPasskey } from "#/modules/approval-session/passkey-statement";
import { useServerList } from "#/modules/machines/use-servers";
import {
  channelApprovalBody,
  channelScope,
  needsApproval,
  type PendingFrom,
  type ReleaseChannel,
  serverUpgradeSettings,
} from "./server-upgrade";
import {
  approveUpgradeChannelServerFn,
  requestServerUpgradeServerFn,
  setServerUpgradeSettingsServerFn,
} from "./server-upgrade.functions";

export const UPGRADE_REFUSED = "Only an owner or admin can upgrade servers.";

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
  const { channel, ensureApproved } = useServerUpgradeSettings(organizationSlug);
  const upgrade = () => {
    pending.start();
    ensureApproved(channel)
      .then((approved) => {
        if (!approved) {
          pending.cancel();
          return;
        }
        return requestServerUpgradeServerFn({ data: { organizationSlug, machineId: server?.id ?? null } });
      })
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
 * saves in the background, and rolls back with a toast. On Servers that enforce device approval, a new channel is the
 * person's passkey answering its own prompt for it, which Cloud keeps and sends with every upgrade.
 */
export function useServerUpgradeSettings(organizationSlug: string) {
  const collection = getServerUpgradeSettingsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const settings = serverUpgradeSettings(rows);
  const scope = channelScope(useServerList(organizationSlug).servers.map(({ machine }) => machine));
  /**
   * Ask the passkey for `channel` and keep its answer, when the Servers enforce and nobody approved it. False when the
   * person dismissed the prompt, which a toast says; throws when Cloud refused the answer.
   */
  const ensureApproved = async (channel: ReleaseChannel) => {
    if (!needsApproval(scope, channel, settings.approval) || scope.cluster === null) return true;
    let approval;
    try {
      approval = await signWithPasskey(channelApprovalBody(scope.cluster, channel, settings.approval), "rule");
    } catch (error) {
      toast.error(error instanceof PasskeyRefused ? error.message : "Your passkey didn't answer. Try again.");
      return false;
    }
    const row = await approveUpgradeChannelServerFn({ data: { organizationSlug, approval } });
    collection.utils.writeUpsert(row);
    return true;
  };
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
    /** The Servers enforce device approval, so a channel change asks for a passkey. */
    asksPasskey: scope.enforced,
    ensureApproved,
    setAutomatic: (automatic: boolean) => {
      if (!automatic) return save({ automatic }, "Could not change automatic upgrades");
      ensureApproved(settings.channel)
        .then((approved) => { if (approved) save({ automatic }, "Could not change automatic upgrades"); })
        .catch(() => toast.error("Could not change automatic upgrades"));
    },
    setChannel: (channel: ReleaseChannel) => {
      if (!scope.enforced) return save({ channel }, "Could not change releases");
      // Also saved when this channel's approval was already kept but an unsigned change moved the setting off it.
      ensureApproved(channel)
        .then((approved) => { if (approved) save({ channel }, "Could not change releases"); })
        .catch(() => toast.error("Could not change releases"));
    },
  };
}
