import { useEffect, useState } from "react";
import { toast } from "sonner";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { type BuildConcurrencyChange, policyChangeObserved, type ServerPolicyChange } from "./server-policy";
import { requestServerPolicyChangeServerFn } from "./server-policy.functions";

/** How long a requested Server Policy change may take to appear in observation. */
const POLICY_APPLY_TIMEOUT_MS = 60_000;

/**
 * Server Policy is read back from Runtime observation. A requested change shows at once and settles when observation
 * catches up; if it never does, the page returns to what the Server reports and says so. `what` names the setting in
 * the toasts: "build settings", "services setting".
 */
export function useServerPolicy(machine: RuntimeMachineRecord, organizationSlug: string, what: string) {
  const [pending, setPending] = useState<ServerPolicyChange | null>(null);
  const settled = pending !== null && policyChangeObserved(machine, pending);

  useEffect(() => {
    if (settled) setPending(null);
  }, [settled]);

  useEffect(() => {
    if (pending === null) return;
    const timer = setTimeout(() => {
      setPending(null);
      toast.error(`${machine.name} has not applied the ${what} yet`);
    }, POLICY_APPLY_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [pending, machine.name, what]);

  function request(change: ServerPolicyChange) {
    setPending((current) => ({ ...current, ...change }));
    requestServerPolicyChangeServerFn({
      data: { organizationSlug, machineId: machine.id, change },
    }).catch(() => {
      setPending(null);
      toast.error(`Could not change ${what} for ${machine.name}`);
    });
  }

  const concurrency: BuildConcurrencyChange =
    pending?.buildConcurrency ?? machine.buildConcurrency ?? "automatic";
  return {
    acceptsBuilds: pending?.acceptsBuilds ?? machine.acceptsBuilds,
    acceptsServices: pending?.acceptsServices ?? machine.acceptsServices,
    concurrency,
    // The Server reports what it enforces; while automatic, that is the automatic value.
    automatic: machine.buildConcurrency === null ? machine.effectiveBuildConcurrency : null,
    request,
  };
}
