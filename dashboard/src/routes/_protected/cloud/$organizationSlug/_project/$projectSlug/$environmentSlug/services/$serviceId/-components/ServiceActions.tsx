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
import { refetchApprovals } from "#/components/agent-sidebar/approvals.queries";
import { actServiceServerFn } from "#/modules/service-act/service-act.functions";
import { serviceActKeys, useLatestServiceAct } from "#/modules/service-act/service-act.queries";
import { actApprovals } from "#/modules/approval-session/approval-prompt";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { StoreService } from "./StoreServiceDrawer";
import { useServiceRuntime } from "./useServiceRuntime";

/** How long a click reads as started before its row arrives; past it, the request was lost. */
const PENDING_MS = 60_000;

type Subject = {
  readonly organizationSlug: string;
  readonly environmentId: string;
  readonly environmentName: string;
  readonly namespace: string | null;
  readonly service: string;
  /** The Environment is Protected and the person can't protect it: an owner or admin approves, from their own browser. */
  readonly asks: boolean;
};

/** What a press acts on as the Servers report it: the Engine's Service name, its copies, and the Servers. */
export type Press = ReturnType<typeof usePress>;

export function usePress(state: StoreService, namespace: string | null) {
  const runtime = useServiceRuntime(state, namespace);
  const { machines } = useRuntimeLens(state.organizationSlug);
  return {
    runtimeService: runtime === null ? null : runtime.identity.slice(runtime.identity.indexOf("/") + 1),
    copies: runtime === null ? [] : [
      ...runtime.containers.map((copy) => ({ ...copy, hook: false })),
      ...runtime.hookContainers.map((copy) => ({ ...copy, hook: true })),
    ],
    machines,
  };
}

/**
 * The request id is minted here so a retried POST admits the same run instead of a second one. A press first asks for a
 * passkey session and carries one approval per copy, unless an owner must approve it.
 */
export function useServiceAct(subject: Subject) {
  const queryClient = useQueryClient();
  const { organizationSlug, environmentId, service } = subject;
  const latest = useLatestServiceAct({ organizationSlug, environmentId, service });
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

  // A press an owner must approve asks nothing of this browser: the owner's passkey approves it with the ask. A start
  // never asks an owner.
  const approvalsFor = (verb: ServiceVerb, press: Press) => {
    if ((subject.asks && verb !== "start") || subject.namespace === null || press.runtimeService === null) return undefined;
    const environment = { environmentId, environmentName: subject.environmentName, namespace: subject.namespace };
    return actApprovals(verb, { environment, runtime: press.runtimeService, copies: press.copies, machines: press.machines });
  };

  const act = async (verb: ServiceVerb, press: Press) => {
    let approvals;
    try {
      approvals = await approvalsFor(verb, press);
    } catch (error) {
      toast.error(toErrorMessage(error, `Could not approve the ${verb}`));
      return;
    }
    if (approvals === null) return;
    const requestId = crypto.randomUUID();
    setRequested({ verb, id: requestId });
    actServiceServerFn({ data: { organizationSlug, environmentId, service, verb, requestId, approvals } }).then(
      (result) => {
        if ("requested" in result) {
          setRequested((current) => (current?.id === requestId ? null : current));
          toast("Requested");
          void refetchApprovals(queryClient, subject.organizationSlug);
          return;
        }
        setRequested((current) => (current?.id === requestId ? { verb, id: result.id } : current));
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

/** `asks`: the Environment is Protected and you can't protect it, so a restart or stop asks an owner or admin first. */
export function ServiceActions({ state, namespace, act, asks }: { state: StoreService; namespace: string | null; act: ServiceAct; asks: boolean }) {
  const [confirming, setConfirming] = useState(false);
  const press = usePress(state, namespace);
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
        <Button size="sm" onClick={() => void act.act("start", press)}>Start</Button>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon" aria-label={`${name} actions`} title="Actions" disabled={act.badge !== null} />}>
          <EllipsisVerticalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuGroup>
            {stopped
              ? <DropdownMenuItem onClick={() => void act.act("start", press)}>Start</DropdownMenuItem>
              : <DropdownMenuItem onClick={() => void act.act("restart", press)}>{asks ? "Request restart" : "Restart"}</DropdownMenuItem>}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuItem variant="destructive" disabled={stopped} onClick={() => setConfirming(true)}>{asks ? "Request stop" : "Stop"}</DropdownMenuItem>
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={`Stop ${name}?`}
        description={`${containers.length === 1 ? "Its container stops" : `All ${plural(containers.length, "container")} stop`} on ${servers === 1 ? "its server" : "every server"} until you start it or deploy.${asks ? " An owner or admin approves it first." : ""}`}
        actionLabel={asks ? "Request stop" : "Stop"}
        variant="destructive"
        onConfirm={() => {
          setConfirming(false);
          void act.act("stop", press);
        }}
      />
    </>
  );
}

export function ServiceActAlert({ act, state, namespace }: { act: ServiceAct; state: StoreService; namespace: string | null }) {
  const press = usePress(state, namespace);
  if (act.alert === null) return null;
  const { title, description, retry } = act.alert;
  return (
    <Alert variant="destructive" className="mt-3">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
      {retry !== null && (
        <AlertAction>
          <Button variant="outline" size="sm" onClick={() => void act.act(retry, press)}>Retry</Button>
        </AlertAction>
      )}
    </Alert>
  );
}
