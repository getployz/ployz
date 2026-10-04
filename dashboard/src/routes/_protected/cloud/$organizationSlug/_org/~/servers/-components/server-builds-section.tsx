import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";
import { Field, FieldContent, FieldDescription, FieldLabel } from "#/components/ui/field";
import { useId } from "react";
import { Link } from "@tanstack/react-router";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { Switch } from "#/components/ui/switch";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import type { BuildConcurrencyChange } from "#/modules/machines/server-policy";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";

const BUILD_CONCURRENCY_CHOICES = [1, 2, 4, 8];

/** One Server's part in builds: whether it takes them, and how many at once. Where builds run first is org-wide. */
export function ServerBuildsSection({ machine, organizationSlug }: { machine: RuntimeMachineRecord; organizationSlug: string }) {
  const policy = useServerPolicy(machine, organizationSlug, "build settings");
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
    <SettingsSection id="server-builds" title="Builds" description={<>
      Which builders go first is set in{" "}
      <Link className="underline underline-offset-4" to="/cloud/$organizationSlug/~/settings" params={{ organizationSlug }} search={{ section: "builds" }}>Organization › Builds</Link>.
    </>}>
        <Field orientation="horizontal">
          <FieldContent>
            <FieldLabel htmlFor={switchId}>Run builds here</FieldLabel>
            {machine.runningBuilds > 0 ? <FieldDescription>Building {machine.runningBuilds} now</FieldDescription> : null}
          </FieldContent>
          <Switch
            id={switchId}
            checked={policy.acceptsBuilds}
            onCheckedChange={(acceptsBuilds) => policy.request({ acceptsBuilds })}
          />
        </Field>
        {policy.acceptsBuilds ? (
          <Field orientation="horizontal">
            <FieldContent>
              <FieldLabel>Builds at once</FieldLabel>
            </FieldContent>
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
          </Field>
        ) : null}
    </SettingsSection>
  );
}
