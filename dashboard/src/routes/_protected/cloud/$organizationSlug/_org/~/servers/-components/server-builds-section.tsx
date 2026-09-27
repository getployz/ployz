import { useEffect, useId, useState } from "react";
import { Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import {
  policyChangeObserved,
  type BuildConcurrencyChange,
  type ServerPolicyChange,
} from "#/modules/machines/server-policy";
import { requestServerPolicyChangeServerFn } from "#/modules/machines/server-policy.functions";

/** How long a requested Server Policy change may take to appear in observation. */
const POLICY_APPLY_TIMEOUT_MS = 60_000;
const BUILD_CONCURRENCY_CHOICES = [1, 2, 4, 8];

/**
 * Server Policy is read back from Runtime observation. A requested change shows
 * at once and settles when observation catches up; if it never does, the page
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

/** One Server's part in builds: whether it takes them, and how many at once. Where builds run first is org-wide. */
export function ServerBuildsSection({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const policy = useServerPolicy(machine, organizationSlug);
  const switchId = useId();
  const choices = [
    ...new Set([
      ...BUILD_CONCURRENCY_CHOICES,
      ...(policy.concurrency === "automatic" ? [] : [policy.concurrency]),
    ]),
  ].sort((left, right) => left - right);
  const label = (value: BuildConcurrencyChange) =>
    value !== "automatic" ? String(value) : policy.automatic === null ? "Auto" : `Auto (${policy.automatic})`;

  return (
    <section aria-labelledby="server-builds-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="server-builds-heading">Builds</h2>
          </ItemTitle>
        </ItemContent>
        <Item variant="outline">
          <ItemContent>
            <ItemTitle>
              <label htmlFor={switchId}>Run builds here</label>
            </ItemTitle>
            {machine.runningBuilds > 0 ? <ItemDescription>Building {machine.runningBuilds} now</ItemDescription> : null}
          </ItemContent>
          <ItemActions>
            <Switch
              id={switchId}
              checked={policy.acceptsBuilds}
              onCheckedChange={(acceptsBuilds) => policy.request({ acceptsBuilds })}
            />
          </ItemActions>
        </Item>
        {policy.acceptsBuilds ? (
          <Item variant="outline">
            <ItemContent>
              <ItemTitle>Builds at once</ItemTitle>
            </ItemContent>
            <ItemActions>
              <Select
                value={String(policy.concurrency)}
                onValueChange={(value) => {
                  if (value === "automatic") policy.request({ buildConcurrency: "automatic" });
                  else if (value) policy.request({ buildConcurrency: Number(value) });
                }}
              >
                <SelectTrigger aria-label="Builds at once" className="w-32">
                  <SelectValue>{label(policy.concurrency)}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    <SelectItem value="automatic" label={label("automatic")}>{label("automatic")}</SelectItem>
                    {choices.map((choice) => (
                      <SelectItem key={choice} value={String(choice)} label={String(choice)}>{choice}</SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
            </ItemActions>
          </Item>
        ) : null}
        <ItemContent>
          <ItemDescription>
            Which builders go first is set in{" "}
            <Link to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} search={{ section: "builds" }}>Settings › Builds</Link>.
          </ItemDescription>
        </ItemContent>
      </ItemGroup>
    </section>
  );
}
