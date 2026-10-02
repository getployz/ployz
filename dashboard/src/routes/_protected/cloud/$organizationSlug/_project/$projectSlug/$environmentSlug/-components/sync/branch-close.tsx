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

type Params = { organizationSlug: string; projectSlug: string; environmentSlug: string };

/** Once the Branch is gone (the Store deletes a closed one), the page opens the Parent it had. */
export function useLeaveWhenClosed(params: Params, branch: StoreResult<BranchView>) {
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
export function useStoreBranchClose(params: Params, store: EnvironmentRef, branch: BranchView) {
  const writer = useStoreWriter(params.organizationSlug);
  const navigate = useNavigate();
  const lossOf = useVolumeLossCheck(params.organizationSlug);
  const place = `${store.project ?? ""}/${store.environment ?? ""}`;
  const [loss, setLoss] = useState<DeletionCheck<Acceptance> | null>(null);
  // Shutting down takes it off the Servers and keeps it; closing then removes it.
  const [shutting, setShutting] = useState(false);
  const name = branch.environment.name;

  const leave = () => navigate(getDashboardDestination({
    kind: "environment", organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, environmentSlug: branch.parent,
  }, "architecture"));

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

  async function shutDown() {
    setShutting(true);
    setLoss(await takeOff({ accept: [], version: null }, true));
  }

  async function close() {
    setShutting(false);
    setLoss(await takeOff({ accept: [], version: null }, false));
  }

  return {
    close,
    shutDown,
    leave: async () => { await leave(); },
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
