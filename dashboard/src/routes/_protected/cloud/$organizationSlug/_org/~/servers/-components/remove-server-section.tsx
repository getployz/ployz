import { useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useStillHere } from "#/hooks/use-still-here";
import { Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import {
  enqueueMachineRemoveServerFn,
  getMachineRemoveAttemptServerFn,
  loadMachineDataLossServerFn,
} from "#/modules/machines/machine-removal.functions";
import { withMissingDataLossIdentities, type DataLossList } from "#/modules/runtime/data-loss-confirm";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import type { MachineRemoveResult } from "#/modules/machines/machine-removal";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";

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
 * the runtime, and once the Server is gone the page returns to Servers.
 */
export function RemoveServerSection({ machine, organizationSlug, last }: {
  machine: RuntimeMachineRecord; organizationSlug: string; last: boolean;
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
    <section aria-labelledby="remove-server-heading">
      <h2 id="remove-server-heading" className="sr-only">Remove server</h2>
      <DangerRow
        title={`Remove ${machine.name}`}
        description={last
          ? "Resets it. It's your only server, so everything running here stops until you add another."
          : "Resets it and takes it out of the cluster. Services that run only here stop."}
        action={
          <Button variant="destructive" className="shrink-0" onClick={() => onOpenChange(true)}>
            <Trash2Icon data-icon="inline-start" />
            Remove server
          </Button>
        }
      />
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
    </section>
  );
}
