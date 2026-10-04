import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { drainView, type DrainView } from "./server-drain";
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
 * when one was already active on this Server; the row then waits on that one. Cloud refusing toasts its words.
 */
export function useServerDrain(
  organizationSlug: string,
  server: { readonly id: string; readonly name: string },
  servers: ReadonlyArray<{ readonly machine: { readonly id: string }; readonly name: string }>,
): ServerDrain {
  const queryClient = useQueryClient();
  const { servers: latest } = useServerDrains(organizationSlug);
  const [requested, setRequested] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const arrived = requested !== null && latest[server.id]?.attemptId === requested;

  useEffect(() => {
    if (arrived) setRequested(null);
  }, [arrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);

  const confirm = () => {
    const requestId = crypto.randomUUID();
    setRequested(requestId);
    setOpen(false);
    requestServerDrainServerFn({ data: { organizationSlug, machineId: server.id, requestId } }).then(
      (admitted) => {
        setRequested((current) => (current === requestId ? admitted.attemptId : current));
        // The change stream brings the row too; this covers a tab whose stream is reconnecting.
        void queryClient.invalidateQueries({ queryKey: serverDrainKeys.all });
      },
      (error: Error) => {
        setRequested((current) => (current === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not drain ${server.name}`));
      },
    );
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
