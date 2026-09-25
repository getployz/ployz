"use client";

import { copyText } from "#/lib/clipboard";

import { useEffect, useRef, useState } from "react";
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
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Field, FieldLabel } from "#/components/ui/field";
import { Switch } from "#/components/ui/switch";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import {
  policyChangeObserved,
  type BuildConcurrencyChange,
  type ServerPolicyChange,
} from "#/modules/machines/server-policy";
import { requestServerPolicyChangeServerFn } from "#/modules/machines/server-policy.functions";
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

/** How long a requested Server Policy change may take to appear in observation. */
const POLICY_APPLY_TIMEOUT_MS = 60_000;
const BUILD_CONCURRENCY_CHOICES = [1, 2, 4, 8];

/**
 * Server Policy is read back from Runtime observation. A requested change shows
 * at once and settles when observation catches up; if it never does, the row
 * returns to what the Server reports and says so.
 */
function useServerPolicy(machine: RuntimeMachineRecord, organizationSlug: string) {
  const [pending, setPending] = useState<ServerPolicyChange | null>(null);
  const settled = pending !== null && policyChangeObserved(machine, pending);

  useEffect(() => {
    if (settled) setPending(null);
  }, [settled]);

  useEffect(() => {
    if (pending === null) return;
    const timer = setTimeout(() => {
      setPending(null);
      toast.error(`${machine.name} has not applied the build settings yet`);
    }, POLICY_APPLY_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [pending, machine.name]);

  function request(change: ServerPolicyChange) {
    setPending((current) => ({ ...current, ...change }));
    requestServerPolicyChangeServerFn({
      data: { organizationSlug, machineId: machine.id, change },
    }).catch(() => {
      setPending(null);
      toast.error(`Could not change build settings for ${machine.name}`);
    });
  }

  const concurrency: BuildConcurrencyChange =
    pending?.buildConcurrency ?? machine.buildConcurrency ?? "automatic";
  return {
    acceptsBuilds: pending?.acceptsBuilds ?? machine.acceptsBuilds,
    concurrency,
    // The Server reports what it enforces; while automatic, that is the automatic value.
    automatic: machine.buildConcurrency === null ? machine.effectiveBuildConcurrency : null,
    request,
  };
}

type ServerPolicy = ReturnType<typeof useServerPolicy>;

function RuntimeMachineRow({
  machine,
  organizationSlug,
}: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
}) {
  const address = machine.publicIp ?? machine.id;
  const policy = useServerPolicy(machine, organizationSlug);

  async function copyAddress() {
    if (await copyText(address)) toast.info("Address copied");
  }

  const description = [
    machine.publicIp ? `public ${machine.publicIp}` : null,
    ...machine.endpoints,
    `${machine.observedContainerCount} containers observed`,
    `observed ${new Date(machine.observedAt).toLocaleString()}`,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Card size="sm">
      <CardHeader className="flex flex-col gap-2 sm:grid">
        <CardTitle className="min-w-0 max-w-full break-words">{machine.name}</CardTitle>
        <CardDescription className="min-w-0 max-w-full break-words">{description || machine.id}</CardDescription>
        <CardAction>
          <div className="flex items-center gap-3">
            {machine.runningBuilds > 0 ? (
              <Badge variant="info">
                {machine.runningBuilds === 1 ? "Building now" : `Building ${machine.runningBuilds} now`}
              </Badge>
            ) : null}
            <Badge variant="outline">
              Membership: {machine.membership.replaceAll("_", " ")}
            </Badge>
            <Field orientation="horizontal">
              <FieldLabel htmlFor={`builds-${machine.id}`}>Builds</FieldLabel>
              <Switch
                id={`builds-${machine.id}`}
                size="sm"
                checked={policy.acceptsBuilds}
                onCheckedChange={(acceptsBuilds) =>
                  policy.request({ acceptsBuilds })
                }
              />
            </Field>
            <MachineActionsMenu
              machine={machine}
              organizationSlug={organizationSlug}
              policy={policy}
              onCopyAddress={() => void copyAddress()}
            />
          </div>
        </CardAction>
      </CardHeader>
    </Card>
  );
}

function MachineActionsMenu({
  machine,
  organizationSlug,
  policy,
  onCopyAddress,
}: {
  machine: RuntimeMachineRecord;
  organizationSlug: string;
  policy: ServerPolicy;
  onCopyAddress: () => void;
}) {
  const choices = [
    ...new Set([
      ...BUILD_CONCURRENCY_CHOICES,
      ...(policy.concurrency === "automatic" ? [] : [policy.concurrency]),
    ]),
  ].sort((left, right) => left - right);

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
            <DropdownMenuLabel>Builds at once</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              value={String(policy.concurrency)}
              onValueChange={(value: string) =>
                policy.request({
                  buildConcurrency:
                    value === "automatic" ? "automatic" : Number(value),
                })
              }
            >
              <DropdownMenuRadioItem value="automatic">
                {policy.automatic === null ? "Automatic" : `Automatic (${policy.automatic})`}
              </DropdownMenuRadioItem>
              {choices.map((choice) => (
                <DropdownMenuRadioItem key={choice} value={String(choice)}>
                  {choice}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
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
