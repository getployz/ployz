import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { EllipsisVerticalIcon } from "lucide-react";
import { toast } from "sonner";
import type { ServiceVerb } from "@ployz/sdk";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { plural } from "#/lib/plural";
import { actAlert, actBadge } from "#/modules/service-act/service-act-view";
import { actServiceServerFn } from "#/modules/service-act/service-act.functions";
import { serviceActKeys, useLatestServiceAct } from "#/modules/service-act/service-act.queries";
import type { StoreService } from "./StoreServiceDrawer";
import { useServiceRuntime } from "./useServiceRuntime";

/** How long a click reads as started before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

type Subject = { readonly organizationSlug: string; readonly environmentId: string; readonly service: string };

/**
 * Restart, stop or start a Service on every Server it runs on. A click reads as started at once and asks Cloud in the
 * background under a request id this tab minted; the run's row then says how far it got, as the change stream brings
 * each container's move. Cloud refusing toasts its words.
 */
export function useServiceAct(subject: Subject) {
  const queryClient = useQueryClient();
  const latest = useLatestServiceAct(subject);
  const [requested, setRequested] = useState<{ readonly verb: ServiceVerb; readonly id: string } | null>(null);
  const arrived = requested !== null && latest?.id === requested.id;

  useEffect(() => {
    if (arrived) setRequested(null);
  }, [arrived]);
  useEffect(() => {
    if (requested === null) return;
    const timer = setTimeout(() => setRequested(null), PENDING_MS);
    return () => clearTimeout(timer);
  }, [requested]);

  const act = (verb: ServiceVerb) => {
    const requestId = crypto.randomUUID();
    setRequested({ verb, id: requestId });
    actServiceServerFn({ data: { ...subject, verb, requestId } }).then(
      ({ id }) => {
        setRequested((current) => (current?.id === requestId ? { verb, id } : current));
        void queryClient.invalidateQueries({ queryKey: serviceActKeys.all });
      },
      (error: Error) => {
        setRequested((current) => (current?.id === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not ${verb} ${subject.service}`));
      },
    );
  };

  const pending = requested !== null && !arrived ? requested.verb : null;
  return { badge: actBadge(latest, pending), alert: pending === null ? actAlert(latest) : null, act };
}

export type ServiceAct = ReturnType<typeof useServiceAct>;

/** The header's run badge, Start button while stopped, and ⋮ menu. Nothing until the Service has containers to move. */
export function ServiceActions({ state, namespace, act }: { state: StoreService; namespace: string | null; act: ServiceAct }) {
  const [confirming, setConfirming] = useState(false);
  const containers = useServiceRuntime(state, namespace)?.containers ?? [];
  const { name } = state.service;
  if (containers.length === 0) return null;
  const stopped = !containers.some((container) => container.runtime?.state === "running");
  const servers = new Set(containers.map((container) => container.machineId)).size;

  return (
    <>
      {act.badge !== null ? (
        <Badge variant="outline"><Spinner data-icon="inline-start" />{act.badge}</Badge>
      ) : stopped ? (
        <Button size="sm" onClick={() => act.act("start")}>Start</Button>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label={`${name} actions`} title="Actions" disabled={act.badge !== null} />}>
          <EllipsisVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuGroup>
            {stopped
              ? <DropdownMenuItem onClick={() => act.act("start")}>Start</DropdownMenuItem>
              : <DropdownMenuItem onClick={() => act.act("restart")}>Restart</DropdownMenuItem>}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuItem variant="destructive" disabled={stopped} onClick={() => setConfirming(true)}>Stop</DropdownMenuItem>
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={`Stop ${name}?`}
        description={`${containers.length === 1 ? "Its container stops" : `All ${plural(containers.length, "container")} stop`} on ${servers === 1 ? "its server" : "every server"} until you start it or deploy.`}
        actionLabel="Stop"
        variant="destructive"
        onConfirm={() => {
          setConfirming(false);
          act.act("stop");
        }}
      />
    </>
  );
}

/** Why the Service's latest run ended short of done, with Retry. */
export function ServiceActAlert({ act }: { act: ServiceAct }) {
  if (act.alert === null) return null;
  const { title, description, verb } = act.alert;
  return (
    <Alert variant="destructive" className="mt-3">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
      <AlertAction>
        <Button variant="outline" size="sm" onClick={() => act.act(verb)}>Retry</Button>
      </AlertAction>
    </Alert>
  );
}
