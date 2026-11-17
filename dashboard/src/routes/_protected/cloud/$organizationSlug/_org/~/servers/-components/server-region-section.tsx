import { useState } from "react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import { isRegion, REGION_RULE } from "#/modules/machines/server-policy";
import { knownRegions } from "#/modules/machines/server-regions";
import type { RoleGate } from "#/modules/organization/members.queries";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { RegionValue } from "./region-value";

const NO_REGION = "_none";
const NEW_REGION = "_new";

export function ServerRegionSection({ machine, machines, organizationSlug, gate }: {
  machine: RuntimeMachineRecord;
  machines: readonly RuntimeMachineRecord[];
  organizationSlug: string;
  gate: RoleGate;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "region");
  const [naming, setNaming] = useState(false);
  const [draft, setDraft] = useState("");
  const choices = knownRegions(machines).filter((region) => region === policy.region || isRegion(region));
  const error = draft === "" || isRegion(draft) ? null : REGION_RULE;

  const choose = (value: string | null) => {
    if (value === NEW_REGION) {
      setNaming(true);
      return;
    }
    const region = value === NO_REGION ? null : value;
    if (region !== policy.region) policy.request({ region });
  };
  const stopNaming = () => {
    setNaming(false);
    setDraft("");
  };
  const submit = () => {
    if (draft === "" || error !== null) return;
    policy.request({ region: draft });
    stopNaming();
  };

  return (
    <SettingsSection id="server-region" title="Region">
      <SettingsRow
        label="Region"
        hint={policy.regionQueued ? "Queued" : undefined}
        commit={naming ? "confirm" : "change"}
        control={naming ? (
          <ConfirmableInput
            aria-label="New region"
            placeholder="fsn"
            autoFocus
            commit="check"
            value={draft}
            onValueChange={setDraft}
            isDirty={draft !== ""}
            error={error}
            onConfirm={submit}
            onCancel={stopNaming}
            onBlur={() => { if (draft === "") stopNaming(); }}
          />
        ) : (
          <Select disabled={!gate.allowed} value={policy.region ?? NO_REGION} onValueChange={choose}>
            <SelectTrigger aria-label="Region" title={gate.reason}>
              <SelectValue>{policy.region === null ? "No region" : <RegionValue region={policy.region} />}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {choices.map((region) => (
                  <SelectItem key={region} value={region} label={region}><RegionValue region={region} /></SelectItem>
                ))}
                {choices.length > 0 ? <SelectSeparator /> : null}
                <SelectItem value={NO_REGION} label="No region">No region</SelectItem>
                <SelectItem value={NEW_REGION} label="New region…">New region…</SelectItem>
              </SelectGroup>
            </SelectContent>
          </Select>
        )}
      />
    </SettingsSection>
  );
}
