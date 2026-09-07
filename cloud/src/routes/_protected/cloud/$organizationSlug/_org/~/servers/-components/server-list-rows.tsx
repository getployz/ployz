"use client";

import { useRef, useState } from "react";
import { CopyIcon, MoreHorizontalIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import {
  MachineRemoveDataLossDialog,
  type DataLossConfirmResult,
} from "#/components/data-loss/data-loss-confirm-dialog";
import { Button } from "#/components/ui/button";
import { Badge } from "#/components/ui/badge";
import {
  Card,
  CardAction,
  CardDescription,
  CardHeader,
  CardTitle,
} from "#/components/ui/card";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import {
  enqueueMachineRemoveServerFn,
  getMachineRemoveAttemptServerFn,
  loadMachineDataLossServerFn,
} from "#/modules/machines/machine-removal.functions";

function waitMs(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve, reject) => {
    if (signal.aborted) {
      reject(new DOMException("Aborted", "AbortError"));
      return;
    }
    const timer = setTimeout(resolve, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(timer);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

async function waitForMachineRemoveAttempt(
  organizationSlug: string,
  attemptId: string,
  signal: AbortSignal,
): Promise<void | DataLossConfirmResult> {
  try {
    for (;;) {
      if (signal.aborted) return;
      const attempt = await getMachineRemoveAttemptServerFn({
        data: { organizationSlug, attemptId },
      });
      if (signal.aborted) return;
      switch (attempt.state) {
        case "pending":
        case "running":
          await waitMs(1_000, signal);
          continue;
        case "succeeded":
          return;
        case "missing_identities":
          return {
            state: "missing_identities",
            identities: attempt.missingIdentities,
          };
        case "failed":
        case "cancelled":
          throw new Error(attempt.failureMessage);
        default: {
          const _exhaustive: never = attempt;
          throw new Error(`Unhandled machine remove attempt: ${_exhaustive}`);
        }
      }
    }
  } catch (error) {
    if (error instanceof DOMException && error.name === "AbortError") return;
    throw error;
  }
}

function RuntimeMachineRow({
  machine,
  organizationSlug,
}: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
}) {
  const address = machine.publicIp ?? machine.overlayIp ?? machine.id;

  async function copyAddress() {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      toast.error("Couldn't access the clipboard");
      return;
    }
    await clipboard.writeText(address);
    toast.info("Address copied");
  }

  const description = [
    machine.publicIp ? `public ${machine.publicIp}` : null,
    machine.overlayIp ? `overlay ${machine.overlayIp}` : null,
    machine.region,
    machine.availabilityZone,
    machine.gateway.status === "not_installed"
      ? null
      : `gateway ${machine.gateway.status.replaceAll("_", " ")}`,
    "routeCount" in machine.gateway
      ? `${machine.gateway.routeCount} routes`
      : null,
    machine.observedContainerCount === null
      ? null
      : `${machine.observedContainerCount} containers`,
    machine.lastObservedAt
      ? `observed ${new Date(machine.lastObservedAt).toLocaleString()}`
      : "no fresh testimony",
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Card size="sm">
      <CardHeader>
        <CardTitle>{machine.name}</CardTitle>
        <CardDescription>{description || machine.id}</CardDescription>
        <CardAction>
          <div className="flex items-center gap-2">
            <Badge
              variant={
                machine.testimonyStatus === "answered"
                  ? "success"
                  : "destructive"
              }
            >
              {machine.testimonyStatus === "answered"
                ? "Responding"
                : "No response"}
            </Badge>
            <RemoveMachineControls
              machine={machine}
              organizationSlug={organizationSlug}
              onCopyAddress={() => void copyAddress()}
            />
          </div>
        </CardAction>
      </CardHeader>
    </Card>
  );
}

function RemoveMachineControls({
  machine,
  organizationSlug,
  onCopyAddress,
}: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
  onCopyAddress: () => void;
}) {
  const [removeOpen, setRemoveOpen] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  function onOpenChange(open: boolean) {
    if (!open) abortRef.current?.abort();
    setRemoveOpen(open);
  }

  return (
    <>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="ghost"
              size="icon"
              aria-label={`Actions for ${machine.name}`}
            />
          }
        >
          <MoreHorizontalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuGroup>
            <DropdownMenuItem onClick={onCopyAddress}>
              <CopyIcon />
              Copy address
            </DropdownMenuItem>
            <DropdownMenuItem
              variant="destructive"
              onClick={() => onOpenChange(true)}
            >
              <Trash2Icon />
              Remove machine
            </DropdownMenuItem>
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
      <MachineRemoveDataLossDialog
        open={removeOpen}
        onOpenChange={onOpenChange}
        confirmPhrase={machine.name}
        callbacks={{
          load: () =>
            loadMachineDataLossServerFn({
              data: {
                organizationSlug,
                machineId: machine.id,
              },
            }),
          confirm: async (rust) => {
            abortRef.current?.abort();
            const abort = new AbortController();
            abortRef.current = abort;
            const queued = await enqueueMachineRemoveServerFn({
              data: {
                organizationSlug,
                machineId: machine.id,
                confirmDataLoss: rust,
              },
            });
            return waitForMachineRemoveAttempt(
              organizationSlug,
              queued.id,
              abort.signal,
            );
          },
        }}
      />
    </>
  );
}

export { RuntimeMachineRow };
