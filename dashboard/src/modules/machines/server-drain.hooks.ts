import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { approvalOptions } from "#/components/agent-sidebar/approvals.queries";
import { toErrorMessage } from "#/lib/error-message";
import { listNames } from "#/lib/plural";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { placementBlockers, placementServers } from "#/modules/volume-placement/daemon-release";
import { stageOffServerServerFn } from "#/modules/volume-placement/volume-placement.functions";
import { volumePlacementKeys } from "#/modules/volume-placement/volume-placement.queries";
import { drainView, type DrainView } from "./server-drain-view";
import { requestServerDrainServerFn } from "./server-drain.functions";
import { serverDrainKeys, useServerDrains } from "./server-drain.queries";

/** How long a click reads as starting before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

export type ServerDrain = {
  readonly view: DrainView;
  /** Open the confirm dialog. */
  readonly ask: () => void;
  readonly dialog: { readonly open: boolean; readonly onOpenChange: (open: boolean) => void; readonly onConfirm: () => void };
};

/**
 * Drain on a Server page: the row reads as starting at once, asks Cloud in the background under a request id this tab
 * minted, and settles when that Drain's row arrives. Cloud answers with the Drain it admitted, which is another tab's
 * when one was already active on this Server; the row then waits on that one. A Drain that would destroy something
 * waits on an approval in the sidebar, and the same request goes again with it once a human approves. Cloud refusing
 * toasts its words.
 * Once Cloud admits the Drain, each Volume primary here is staged to move off with it, for the next Deploy; one that
 * nowhere takes toasts. While a Server with storage runs an older daemon, nothing is staged and every primary stays.
 */
export function useServerDrain(
  organizationSlug: string,
  server: { readonly id: string; readonly name: string },
  servers: ReadonlyArray<{ readonly machine: { readonly id: string }; readonly name: string }>,
): ServerDrain {
  const queryClient = useQueryClient();
  const { servers: latest } = useServerDrains(organizationSlug);
  const { machines } = useRuntimeLens(organizationSlug);
  const [requested, setRequested] = useState<string | null>(null);
  const [asking, setAsking] = useState<{ readonly requestId: string; readonly approvalId: string } | null>(null);
  const [open, setOpen] = useState(false);
  const arrived = requested !== null && latest[server.id]?.attemptId === requested;
  const { data: approval } = useQuery({ ...approvalOptions(organizationSlug, asking?.approvalId ?? ""), enabled: asking !== null });
  const decided = asking !== null && approval?.id === asking.approvalId ? approval.status : null;

  useEffect(() => {
    if (arrived) setRequested(null);
  }, [arrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);

  const stageOff = () => {
    if (placementBlockers(machines).length > 0) return;
    const accepting = [...placementServers(machines)].filter((id) => id !== server.id);
    stageOffServerServerFn({ data: { organizationSlug, server: server.id, accepting } }).then(
      ({ stuck }) => {
        if (stuck.length > 0) toast.error(`${listNames(stuck)} ${stuck.length === 1 ? "has" : "have"} nowhere to move`);
        void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
      },
      (error: Error) => toast.error(toErrorMessage(error, `Could not move the volumes off ${server.name}`)),
    );
  };

  const send = (requestId: string, approvalId: string | null) => {
    setRequested(requestId);
    requestServerDrainServerFn({ data: { organizationSlug, machineId: server.id, requestId, approvalId } }).then(
      (answer) => {
        if ("asking" in answer) {
          setRequested((current) => (current === requestId ? null : current));
          setAsking({ requestId, approvalId: answer.asking.approvalId });
          toast.info(`Draining ${server.name} waits for approval in the sidebar.`);
          return;
        }
        setRequested((current) => (current === requestId ? answer.drain.attemptId : current));
        stageOff();
        // The change stream brings the row too; this covers a tab whose stream is reconnecting.
        void queryClient.invalidateQueries({ queryKey: serverDrainKeys.all });
      },
      (error: Error) => {
        setRequested((current) => (current === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not drain ${server.name}`));
      },
    );
  };

  useEffect(() => {
    if (asking === null || decided === null || decided === "pending") return;
    setAsking(null);
    if (decided === "approved") send(asking.requestId, asking.approvalId);
    else toast.info(decided === "denied" ? `Draining ${server.name} was denied.` : `${server.name} changed before the drain was approved. Drain again.`);
  }, [decided]);

  const confirm = () => {
    setOpen(false);
    send(crypto.randomUUID(), null);
  };

  return {
    view: drainView({
      server,
      latest,
      serverNames: new Map(servers.map((other) => [other.machine.id, other.name])),
      requested,
    }),
    ask: () => setOpen(true),
    dialog: { open, onOpenChange: setOpen, onConfirm: confirm },
  };
}
