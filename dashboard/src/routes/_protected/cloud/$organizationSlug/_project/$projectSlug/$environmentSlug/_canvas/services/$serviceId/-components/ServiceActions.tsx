import { useState } from "react";
import { EllipsisVerticalIcon } from "lucide-react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Alert, AlertAction, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "#/components/ui/dropdown-menu";
import { Spinner } from "#/components/ui/spinner";
import { plural } from "#/lib/plural";
import type { ServiceAct } from "#/modules/service-act/service-act.hooks";
import type { StoreService } from "./StoreServiceDrawer";
import { useServiceRuntime } from "./useServiceRuntime";

/** `asks`: the Environment is Protected and you can't protect it, so a restart or stop asks an owner or admin first. */
export function ServiceActions({ state, namespace, act, asks }: { state: StoreService; namespace: string | null; act: ServiceAct; asks: boolean }) {
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
              : <DropdownMenuItem onClick={() => act.act("restart")}>{asks ? "Request restart" : "Restart"}</DropdownMenuItem>}
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
          act.act("stop");
        }}
      />
    </>
  );
}

export function ServiceActAlert({ act }: { act: ServiceAct }) {
  if (act.alert === null) return null;
  const { title, description, retry } = act.alert;
  return (
    <Alert variant="destructive" className="mt-3">
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{description}</AlertDescription>
      {retry !== null && (
        <AlertAction>
          <Button variant="outline" size="sm" onClick={() => act.act(retry)}>Retry</Button>
        </AlertAction>
      )}
    </Alert>
  );
}
