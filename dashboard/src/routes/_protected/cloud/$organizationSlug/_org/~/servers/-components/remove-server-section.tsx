import { useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
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
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { DangerRow } from "#/routes/_protected/cloud/$organizationSlug/-components/danger-row";

/** Polls the removal until it settles. Closing the dialog aborts; polling stops within a second. */
async function waitForMachineRemoveAttempt(
  organizationSlug: string,
  attemptId: string,
  signal: AbortSignal,
): Promise<"removed" | "aborted" | { missing: DataLossIdentity[] }> {
  for (;;) {
    if (signal.aborted) return "aborted";
    const attempt = await getMachineRemoveAttemptServerFn({
      data: { organizationSlug, attemptId },
    });
    if (signal.aborted) return "aborted";
    switch (attempt.state) {
      case "pending":
      case "running":
        await new Promise((resolve) => setTimeout(resolve, 1_000));
        continue;
      case "succeeded":
        return "removed";
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
 * Removing a Server resets it and takes it out of the cluster; its volumes stay on its disk, unused. It waits on the
 * runtime, and once the Server is gone the page returns to Servers.
 */
export function RemoveServerSection({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const identities = useRef<DataLossList["rust"]>([]);

  function onOpenChange(next: boolean) {
    if (!next) abortRef.current?.abort();
    setOpen(next);
  }

  return (
    <section aria-labelledby="remove-server-heading">
      <h2 id="remove-server-heading" className="sr-only">Remove server</h2>
      <DangerRow
        title={`Remove ${machine.name}`}
        description="Resets it and takes it out of the cluster. Services that run only here stop."
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
        sentence={<>Its volumes stay on its disk, but your services <span className="text-destructive">lose</span> them.</>}
        callbacks={{
          load: async () => {
            const dataLoss = await loadMachineDataLossServerFn({
              data: { organizationSlug, machineId: machine.id },
            });
            identities.current = dataLoss.rust;
            return volumes(dataLoss.rust);
          },
          confirm: async () => {
            abortRef.current?.abort();
            const abort = new AbortController();
            abortRef.current = abort;
            const queued = await enqueueMachineRemoveServerFn({
              data: { organizationSlug, machineId: machine.id, confirmDataLoss: identities.current },
            });
            const result = await waitForMachineRemoveAttempt(organizationSlug, queued.id, abort.signal);
            if (result === "aborted") return;
            if (result === "removed") {
              toast(`${machine.name} removed`);
              void navigate({ to: "/cloud/$organizationSlug/~/servers", params: { organizationSlug } });
              return;
            }
            identities.current = withMissingDataLossIdentities({ rust: identities.current, cloud: [] }, result.missing).rust;
            return volumes(identities.current);
          },
        }}
      />
    </section>
  );
}
