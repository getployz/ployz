import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentRef } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck } from "#/components/deletion-dialog";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck, type VolumeAcceptance as Acceptance } from "#/modules/config-store/use-volume-loss-check";
import { useStoreWriter } from "#/modules/config-store/store-write";
import type { EnvironmentRouteParams } from "../environment-route-paths";

/** Once the Branch is gone (the Store deletes a closed one), the page opens the Parent it had. */
export function useLeaveWhenClosed(params: EnvironmentRouteParams, branch: StoreResult<BranchView>) {
  const navigate = useNavigate();
  const parent = useRef<string | null>(null);
  const gone = !branch.ok && branch.refusal.code === "not_found";
  useEffect(() => {
    if (branch.ok) parent.current = branch.value.parent;
  }, [branch]);
  useEffect(() => {
    if (!gone || parent.current === null) return;
    toast.success(`${params.environmentSlug} closed`);
    void navigate(getDashboardDestination({ ...params, kind: "environment", environmentSlug: parent.current }, "architecture"));
  }, [gone]);
}

/**
 * Closing a Branch over the Config Store: it comes off the Servers as a Deployment that asks before deleting Volume
 * data and that closes it; never deployed, or already off, that applies at once. The Store deletes it then, and the
 * page opens its Parent once it's gone.
 */
export function useStoreBranchClose(params: EnvironmentRouteParams, store: EnvironmentRef, branch: BranchView) {
  const writer = useStoreWriter(params.organizationSlug);
  const lossOf = useVolumeLossCheck(params.organizationSlug);
  const place = `${store.project ?? ""}/${store.environment ?? ""}`;
  const [loss, setLoss] = useState<DeletionCheck<Acceptance> | null>(null);
  // Shutting down takes it off the Servers and keeps it; closing then removes it.
  const [shutting, setShutting] = useState(false);
  const name = branch.environment.name;

  async function takeOff({ accept, version }: { accept: readonly string[]; version: string | null }, shut = shutting): Promise<DeletionCheck<Acceptance> | null> {
    try {
      await writer.commit({
        command: "admit", admit: "remove", id: crypto.randomUUID(), environment: store, version, accept_volume_loss: [...accept],
        close: !shut,
      }, ["confirmation_required"]).isPersisted.promise;
      toast(`${name} is coming off the servers`,
        { description: shut ? "The pull request's next push brings it back." : "It closes once it's off." });
      return null;
    } catch (error) {
      return error instanceof StoreRefused ? lossOf(error) : null;
    }
  }

  /** Shuts it down (`shut`), or closes it. */
  async function start(shut: boolean) {
    setShutting(shut);
    setLoss(await takeOff({ accept: [], version: null }, shut));
  }

  return {
    start,
    dialog: (
      <DeletionDialog
        open={loss !== null}
        onOpenChange={(open) => { if (!open) setLoss(null); }}
        title={shutting ? "Shutting down deletes data" : "Closing deletes data"}
        place={place}
        confirmLabel={shutting ? "Shut down" : "Close branch"}
        items={loss?.items}
        callbacks={{
          load: () => Promise.resolve(loss ?? { items: [], evidence: { accept: [], version: "" } }),
          confirm: async (evidence) => {
            return (await takeOff(evidence)) ?? undefined;
          },
        }}
      />
    ),
  };
}
