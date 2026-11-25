import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import type { DataPlan } from "./server-drain-volumes";
import { drainView, type DrainView } from "./server-drain-view";
import { requestServerDrainServerFn } from "./server-drain.functions";
import { serverDrainPreview, serverDrains } from "./server-drain.queries";
import { usePendingAttempt } from "#/hooks/use-pending-attempt";

export type ServerDrain = {
  readonly view: DrainView;
  readonly ask: () => void;
  readonly dialog: { readonly open: boolean; readonly onOpenChange: (open: boolean) => void; readonly onConfirm: () => void };
  /** What a Drain would do with each Service its data holds here, read while the dialog is open; empty until read. */
  readonly data: Readonly<Record<string, DataPlan>>;
};

/**
 * Drain on a Server page: the row reads as starting at once, asks Cloud in the background under a request id this tab
 * minted, and settles when that Drain's row arrives. Cloud answers with the Drain it admitted, which is another tab's
 * when one was already active on this Server; the row then waits on that one. Cloud refusing toasts its words.
 * While the dialog is open it reads which Services would move with their data.
 */
export function useServerDrain(
  organizationSlug: string,
  server: { readonly id: string; readonly name: string },
  servers: ReadonlyArray<{ readonly machine: { readonly id: string }; readonly name: string }>,
): ServerDrain {
  const queryClient = useQueryClient();
  const { servers: latest } = serverDrains.useLatest(organizationSlug);
  const [requested, setRequested] = usePendingAttempt<string, string | undefined>(
    latest[server.id]?.attemptId,
    (attemptId, latestId) => latestId === attemptId,
  );
  const [open, setOpen] = useState(false);
  const preview = useQuery({ ...serverDrainPreview(organizationSlug, server.id), enabled: open });

  const send = (requestId: string) => {
    setRequested(requestId);
    requestServerDrainServerFn({ data: { organizationSlug, machineId: server.id, requestId } }).then(
      (answer) => {
        setRequested((current) => (current === requestId ? answer.drain.attemptId : current));
        // The change stream brings the row too; this covers a tab whose stream is reconnecting.
        void queryClient.invalidateQueries({ queryKey: serverDrains.keys.all });
      },
      (error: Error) => {
        setRequested((current) => (current === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not drain ${server.name}`));
      },
    );
  };

  const confirm = () => {
    setOpen(false);
    send(crypto.randomUUID());
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
    data: preview.data?.data ?? {},
  };
}
