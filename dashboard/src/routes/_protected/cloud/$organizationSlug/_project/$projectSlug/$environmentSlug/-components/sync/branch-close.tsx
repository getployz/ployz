import { useEffect, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import type { BranchView, EnvironmentRef } from "@ployz/sdk";
import { DeletionDialog, type DeletionCheck, type DeletionItem } from "#/components/deletion-dialog";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useVolumeLossCheck } from "#/modules/config-store/use-volume-loss-check";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { removeTarget, type RemovalConsent } from "#/modules/config-store/store-workspace";
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

/** The Volumes a shutdown accepts losing, at the version the Store asked about (none before it asked). */
type Acceptance = { accept: readonly string[]; version: string | null };

/** What the user confirmed: a shutdown's accepted Volumes, or a close's whole removal consent. */
type Consent = { shut: true; acceptance: Acceptance } | { shut: false; consent: RemovalConsent };

/**
 * Closing a Branch over the Config Store: one `remove`, which asks before deleting Volume data and which the Store
 * finishes, deleting it once it's off the Servers; the page opens its Parent once it's gone. Shutting it down takes it
 * off the Servers and keeps it, asking the same.
 */
export function useStoreBranchClose(params: EnvironmentRouteParams, store: EnvironmentRef, branch: BranchView) {
  const writer = useStoreWriter(params.organizationSlug);
  const lossOf = useVolumeLossCheck(params.organizationSlug);
  const place = `${store.project ?? ""}/${store.environment ?? ""}`;
  const [loss, setLoss] = useState<DeletionCheck<Consent> | null>(null);
  const name = branch.environment.name;
  const commit = (command: Parameters<typeof writer.commit>[0], handles: readonly string[]) =>
    writer.commit(command, handles).isPersisted.promise;

  async function shutDown({ accept, version }: Acceptance): Promise<DeletionCheck<Consent> | null> {
    try {
      await commit({
        command: "admit", admit: "shut_down", id: crypto.randomUUID(), environment: store, version, accept_volume_loss: [...accept],
      }, ["confirmation_required"]);
      toast(`${name} is coming off the servers`, { description: "The pull request's next push brings it back." });
      return null;
    } catch (error) {
      const check = error instanceof StoreRefused ? lossOf(error) : null;
      return check && { items: check.items, evidence: { shut: true, acceptance: check.evidence } };
    }
  }

  async function close(consent: RemovalConsent): Promise<DeletionCheck<Consent> | null> {
    const answer = await removeTarget(commit, { project: store.project ?? "", environment: name }, consent);
    if ("sent" in answer) {
      toast(`${name} is coming off the servers`, { description: "It closes once it's off." });
      return null;
    }
    const items = [
      ...Object.values(answer.ask.accept).flat().map((volume): DeletionItem => ({ kind: "volume", name: volume })),
      ...answer.waitingOn.map((waiting): DeletionItem => ({ kind: "server", name: waiting.server, detail: waiting.why })),
    ];
    return { items, evidence: { shut: false, consent: answer.ask } };
  }

  const take = (consent: Consent) => consent.shut ? shutDown(consent.acceptance) : close(consent.consent);
  const shutting = loss?.evidence.shut === true;

  /** Shuts it down (`shut`), or closes it. */
  async function start(shut: boolean) {
    setLoss(await take(shut ? { shut: true, acceptance: { accept: [], version: null } } : { shut: false, consent: { accept: {}, version: null, everyCopy: {} } }));
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
          load: () => Promise.resolve(loss ?? { items: [], evidence: { shut: true as const, acceptance: { accept: [], version: null } } }),
          confirm: async (evidence) => (await take(evidence)) ?? undefined,
        }}
      />
    ),
  };
}
