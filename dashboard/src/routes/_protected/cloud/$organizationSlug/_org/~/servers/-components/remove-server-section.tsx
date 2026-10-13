import { useEffect, useRef, useState, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useStillHere } from "#/hooks/use-still-here";
import { Trash2Icon, UnplugIcon } from "lucide-react";
import { toast } from "sonner";
import { approvalOptions } from "#/components/agent-sidebar/approvals.queries";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Button } from "#/components/ui/button";
import { ForgetServersDialog } from "../../../-components/forget-servers-dialog";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import {
  enqueueMachineRemoveServerFn,
  forgetServerServerFn,
  getMachineRemoveAttemptServerFn,
  loadMachineDataLossServerFn,
} from "#/modules/machines/machine-removal.functions";
import { withMissingDataLossIdentities, type DataLossList } from "#/modules/runtime/data-loss-confirm";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import type { MachineRemoveResult } from "#/modules/machines/machine-removal";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";
import { RowWarning } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { listNames, plural } from "#/lib/plural";
import type { RemoveHint } from "#/modules/machines/server-drain-view";

/** Polls the removal until it settles. Closing the dialog or leaving the page aborts; polling stops within a second. */
async function waitForMachineRemoveAttempt(
  organizationSlug: string,
  attemptId: string,
  stopped: () => boolean,
): Promise<MachineRemoveResult | "aborted" | { missing: DataLossIdentity[] }> {
  for (;;) {
    if (stopped()) return "aborted";
    const attempt = await getMachineRemoveAttemptServerFn({
      data: { organizationSlug, attemptId },
    });
    if (stopped()) return "aborted";
    switch (attempt.state) {
      case "pending":
      case "running":
        await new Promise((resolve) => setTimeout(resolve, 1_000));
        continue;
      case "succeeded":
        return attempt.result;
      case "missing_identities":
        return { missing: attempt.missingIdentities };
      case "failed":
      case "cancelled":
        throw new Error(attempt.failureMessage);
      default: {
        const _exhaustive: never = attempt;
        throw new Error(`Unhandled machine remove attempt: ${_exhaustive}`);
      }
    }
  }
}

const volumes = (rust: DataLossList["rust"]) =>
  rust.map((identity): DeletionItem => ({ kind: "volume", name: identity.id.name }));

/**
 * Removing a Server resets it and takes it out of the cluster; its volumes stay on its disk, unused. The `last` one takes
 * everything running with it, and Cloud lets go of the cluster: Deploy asks for a server until one is added. It waits on
 * the runtime, and once the Server is gone the page returns to Servers. An `offline` Server can't be reset, so it is
 * forgotten instead: by another Server, or with Forget Servers when it is the last.
 */
export function RemoveServerSection({ machine, organizationSlug, last, offline, hint }: {
  machine: RuntimeMachineRecord; organizationSlug: string; last: boolean; offline: boolean;
  /** What still runs here, and the Drain button that would move it. */
  hint: ReactNode;
}) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const markHere = useStillHere();
  const abortRef = useRef<AbortController | null>(null);

  function onOpenChange(next: boolean) {
    if (!next) abortRef.current?.abort();
    setOpen(next);
  }

  return (
    <>
    <SettingsSection id="remove-server" title="Danger">
      {offline ? last
        ? <ForgetLastServerRow machine={machine} organizationSlug={organizationSlug} />
        : <ForgetServerRow machine={machine} organizationSlug={organizationSlug} /> : <DangerRow
        title={`Remove ${machine.name}`}
        description={last
          ? "Resets it. It's your only server, so everything running here stops until you add another."
          : "Resets it and takes it out of the cluster. Services that run only here stop."}
        action={
          <Button variant="destructive" onClick={() => onOpenChange(true)}>
            <Trash2Icon data-icon="inline-start" />
            Remove server
          </Button>
        }
      >
        {hint}
      </DangerRow>}
    </SettingsSection>
      <DeletionDialog
        open={open}
        onOpenChange={onOpenChange}
        title={`Remove ${machine.name}?`}
        place={machine.name}
        confirmLabel="Remove"
        sentence={<>
          {last ? "Everything running on it stops. " : null}
          Its volumes stay on its disk, but your services <span className="text-destructive">lose</span> them.
        </>}
        callbacks={{
          load: async () => {
            const { rust } = await loadMachineDataLossServerFn({
              data: { organizationSlug, machineId: machine.id },
            });
            return { items: volumes(rust), evidence: rust };
          },
          confirm: async (identities) => {
            abortRef.current?.abort();
            const abort = new AbortController();
            abortRef.current = abort;
            // Leaving the page stops the wait too, checked once the enqueue returns and after every poll. It's the location,
            // not the mount: this section unmounts once the Server leaves the runtime, and that user should still land on Servers.
            const stillHere = markHere();
            const queued = await enqueueMachineRemoveServerFn({
              data: { organizationSlug, machineId: machine.id, confirmDataLoss: identities },
            });
            const result = await waitForMachineRemoveAttempt(organizationSlug, queued.id,
              () => abort.signal.aborted || !stillHere());
            if (result === "aborted") return;
            if ("release" in result) {
              const { release } = result;
              if (release.kind === "kept") toast.warning(`${machine.name} left the cluster. Cloud keeps its hold: ${release.reason}`);
              else if (release.kind === "released") toast(`${machine.name} removed. Add a server to deploy again.`);
              else toast(`${machine.name} removed`);
              void navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug } });
              return;
            }
            const { rust } = withMissingDataLossIdentities({ rust: identities, cloud: [] }, result.missing);
            return { items: volumes(rust), evidence: rust };
          },
        }}
      />
  </>
  );
}

/** No other Server is left to forget the last one, so Forget Servers forgets the Cluster with it. */
function ForgetLastServerRow({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <DangerRow
        title={`Forget ${machine.name}`}
        description="It's gone, and it's your only server. Your projects and settings stay; its volumes can't be recovered."
        action={
          <Button variant="destructive" onClick={() => setOpen(true)}>
            <UnplugIcon data-icon="inline-start" />
            Forget server
          </Button>
        }
      />
      <ForgetServersDialog organizationSlug={organizationSlug} open={open} onOpenChange={setOpen} />
    </>
  );
}

/**
 * Forgetting takes a Server that is gone for good out of the cluster without resetting it. It asks for approval in the
 * sidebar when volumes go with it, sends the same request again once a human approves, then waits on the removal.
 */
function ForgetServerRow({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const navigate = useNavigate();
  const markHere = useStillHere();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState<{ readonly requestId: string; readonly approvalId: string } | null>(null);
  const { data: approval } = useQuery({ ...approvalOptions(organizationSlug, asking?.approvalId ?? ""), enabled: asking !== null });
  const decided = asking !== null && approval?.id === asking.approvalId ? approval.status : null;

  const send = async (requestId: string, approvalId: string | null) => {
    setBusy(true);
    const stillHere = markHere();
    try {
      const answer = await forgetServerServerFn({ data: { organizationSlug, machineId: machine.id, requestId, approvalId } });
      if (answer.asking !== undefined) {
        setAsking({ requestId, approvalId: answer.asking.approvalId });
        toast.info(`Forgetting ${machine.name} waits for approval in the sidebar.`);
        return;
      }
      const result = await waitForMachineRemoveAttempt(organizationSlug, answer.id, () => !stillHere());
      if (result === "aborted") return;
      if ("missing" in result) throw new Error(`${machine.name} changed while it was being forgotten. Try again.`);
      toast(`${machine.name} forgotten`);
      void navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug } });
    } catch (error) {
      toast.error(toErrorMessage(error, `Could not forget ${machine.name}`));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    if (asking === null || decided === null || decided === "pending") return;
    setAsking(null);
    if (decided === "approved") void send(asking.requestId, asking.approvalId);
    else toast.info(decided === "denied" ? `Forgetting ${machine.name} was denied.` : `${machine.name} changed before it was approved. Forget it again.`);
  }, [decided]);

  return (
    <>
      <DangerRow
        title={`Forget ${machine.name}`}
        description="It's gone. Ployz stops waiting for it; its volumes can't be recovered."
        action={
          <Button variant="destructive" disabled={busy || asking !== null} onClick={() => setOpen(true)}>
            {busy ? <Spinner data-icon="inline-start" /> : <UnplugIcon data-icon="inline-start" />}
            Forget server
          </Button>
        }
      />
      <AlertDialog open={open} onOpenChange={setOpen}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Forget {machine.name}?</AlertDialogTitle>
            <AlertDialogDescription>
              Ployz takes it out of the cluster without reaching it. Its volumes{" "}
              <span className="text-destructive">can't be recovered</span>.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => { setOpen(false); void send(crypto.randomUUID(), null); }}>Forget</AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </>
  );
}

/**
 * While Containers remain here, Remove suggests draining first. When the latest Drain left only Services whose data
 * is on this Server, draining again won't move them, so it says why instead. Services no Project owns get their own
 * line: a Drain leaves them.
 */
export function RemoveServerHint({ hint, drainButton }: { hint: RemoveHint; drainButton: ReactNode }) {
  return (
    <>
      <DrainableHint hint={hint} drainButton={drainButton} />
      {hint.unowned.length === 0 ? null : (
        <RowWarning>
          {listNames([...hint.unowned])} still {hint.unowned.length === 1 ? "runs" : "run"} here. No project owns{" "}
          {hint.unowned.length === 1 ? "it" : "them"}, so Drain leaves {hint.unowned.length === 1 ? "it" : "them"}.
        </RowWarning>
      )}
    </>
  );
}

function DrainableHint({ hint, drainButton }: { hint: RemoveHint; drainButton: ReactNode }) {
  switch (hint.kind) {
    case "none":
      return null;
    case "pinned": {
      const one = hint.names.length === 1;
      return (
        <RowWarning>
          {listNames([...hint.names])} still {one ? "runs" : "run"} here. {one ? "Its volume is" : "Their volumes are"} on this server.
        </RowWarning>
      );
    }
    case "drain":
      return (
        <RowWarning action={drainButton}>
          {plural(hint.count, "service")} still {hint.count === 1 ? "runs" : "run"} here. Drain first to move them.
        </RowWarning>
      );
  }
}
