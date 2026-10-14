import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeStatus } from "#/providers/runtime-provider";
import type { WebTrafficWant } from "./server-web-traffic";
import type { ServerStatus } from "./server-status";
import { lastWebTrafficServer, WEB_TRAFFIC_SETTLE_MS, webTrafficView, type WebTrafficView } from "./server-web-traffic-view";
import { requestServerWebTrafficServerFn } from "./server-web-traffic.functions";
import { serverWebTrafficKeys, useServerWebTrafficChanges } from "./server-web-traffic.queries";

/** How long a flip reads as under way before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

export type ServerWebTraffic = {
  readonly view: WebTrafficView;
  /** The domains that point at Servers taking web traffic: what turning off here affects. */
  readonly domains: readonly string[];
  /** Some Server's certificates weren't observed, so `domains` may miss some. */
  readonly partial: boolean;
  /** No other online Server takes web traffic: turning off here stops every domain. */
  readonly last: boolean;
  /** Turn on now (the switch, and Retry). */
  readonly turnOn: () => void;
  /** Open the confirm before turning off. */
  readonly ask: () => void;
  readonly dialog: { readonly open: boolean; readonly onOpenChange: (open: boolean) => void; readonly onConfirm: () => void };
};

/**
 * The Web traffic switch on a Server page. A flip reads as under way at once, asks Cloud in the background under a
 * request id this tab minted, and settles when that change's row arrives. Cloud answers with the change it admitted,
 * which is another tab's when one was already active on this Server; the row then waits on that one. Cloud refusing
 * toasts its words.
 */
export function useServerWebTraffic(
  organizationSlug: string,
  server: { readonly id: string; readonly name: string },
  machine: RuntimeMachineRecord,
  servers: ReadonlyArray<{ readonly machine: RuntimeMachineRecord; readonly status: ServerStatus }>,
): ServerWebTraffic {
  const queryClient = useQueryClient();
  const { servers: latest } = useServerWebTrafficChanges(organizationSlug);
  const { certificates, incompleteIds } = useRuntimeStatus();
  const [requested, setRequested] = useState<{ readonly id: string; readonly want: WebTrafficWant } | null>(null);
  const [open, setOpen] = useState(false);
  const [settledId, setSettledId] = useState<string | null>(null);
  const own = latest[server.id] ?? null;
  const arrived = requested !== null && own?.attemptId === requested.id;
  const finished = own !== null && own.state === "finished" ? own : null;

  useEffect(() => {
    if (arrived) setRequested(null);
  }, [arrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);
  useEffect(() => {
    if (finished === null) return;
    const left = new Date(finished.endedAt).getTime() + WEB_TRAFFIC_SETTLE_MS - Date.now();
    if (left <= 0) {
      setSettledId(finished.attemptId);
      return;
    }
    const timer = setTimeout(() => setSettledId(finished.attemptId), left);
    return () => clearTimeout(timer);
  }, [finished]);

  const send = (want: WebTrafficWant) => {
    const requestId = crypto.randomUUID();
    setRequested({ id: requestId, want });
    requestServerWebTrafficServerFn({ data: { organizationSlug, machineId: server.id, requestId, want } }).then(
      (answer) => {
        setRequested((current) => (current?.id === requestId ? { id: answer.change.attemptId, want: answer.change.want } : current));
        // The change stream brings the row too; this covers a tab whose stream is reconnecting.
        void queryClient.invalidateQueries({ queryKey: serverWebTrafficKeys.all });
      },
      (error: Error) => {
        setRequested((current) => (current?.id === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not turn web traffic ${want} for ${server.name}`));
      },
    );
  };

  return {
    view: webTrafficView({
      server: { id: server.id, acceptsIngress: machine.acceptsIngress, publicIp: machine.publicIp },
      latest,
      requested,
      settled: finished !== null && settledId === finished.attemptId,
    }),
    domains: [...new Set(certificates.map((certificate) => certificate.hostname))],
    partial: incompleteIds.certificates.length > 0,
    last: lastWebTrafficServer({ id: server.id, acceptsIngress: machine.acceptsIngress, publicIp: machine.publicIp }, servers),
    turnOn: () => send("on"),
    ask: () => setOpen(true),
    dialog: {
      open,
      onOpenChange: setOpen,
      onConfirm: () => {
        setOpen(false);
        send("off");
      },
    },
  };
}
