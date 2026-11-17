import type { Region } from "@ployz/sdk";
import { useEffect, useState } from "react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import { regionLabels, regionName, regionOf } from "#/modules/machines/server-regions";
import { useNameRegion, useNewRegions } from "#/modules/machines/server-regions.hooks";
import type { RoleGate } from "#/modules/organization/members.queries";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";

// Menu values outside the label grammar, so no region's key collides with them.
const NO_REGION = "no region";
const NEW_REGION = "new region";

export function ServerRegionSection({ machine, machines, regions, organizationSlug, gate }: {
  machine: RuntimeMachineRecord;
  machines: readonly RuntimeMachineRecord[];
  /** The Organization's regions; undefined until their names arrive. */
  regions: readonly Region[] | undefined;
  organizationSlug: string;
  gate: RoleGate;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "region");
  const nameRegion = useNameRegion(organizationSlug);
  const [naming, setNaming] = useState(false);
  const [draft, setDraft] = useState("");
  // Enter before the Store has checked the name moves the Server once it has.
  const [confirming, setConfirming] = useState(false);
  const check = useNewRegions(organizationSlug, naming ? [draft] : [], regionLabels(machines))(draft);
  const choices = regions ?? [];
  const label = policy.region;

  const choose = (value: string | null) => {
    if (value === NEW_REGION) {
      setNaming(true);
      return;
    }
    const region = value === NO_REGION ? null : value;
    if (region !== label) policy.request({ region });
  };
  const stopNaming = () => {
    setNaming(false);
    setDraft("");
    setConfirming(false);
  };
  const create = check.kind === "new" ? { key: check.key, name: draft.trim() } : null;
  const createIn = (region: Region) => {
    policy.request({ region: region.key });
    void nameRegion(region);
    stopNaming();
  };

  useEffect(() => {
    if (confirming && create !== null) createIn(create);
  }, [confirming, create?.key]);

  return (
    <SettingsSection id="server-region" title="Placement">
      <SettingsRow
        label="Region"
        hint={policy.regionQueued ? "Queued" : undefined}
        commit={naming ? "confirm" : "change"}
        control={naming ? (
          <ConfirmableInput
            aria-label="New region"
            placeholder="Sydney"
            autoFocus
            commit="check"
            value={draft}
            onValueChange={(value) => {
              setDraft(value);
              setConfirming(false);
            }}
            isDirty={draft.trim() !== ""}
            error={check.kind === "refused" && draft.trim() !== "" ? check.message : null}
            onConfirm={() => (create === null ? setConfirming(true) : createIn(create))}
            onCancel={stopNaming}
            onBlur={() => { if (draft === "") stopNaming(); }}
          />
        ) : (
          <Select disabled={!gate.allowed} value={label === null ? NO_REGION : regionOf(label, choices)?.key ?? label} onValueChange={choose}>
            <SelectTrigger aria-label="Region" title={gate.reason}>
              <SelectValue>{label === null ? "No region" : regionName(label, regions)}</SelectValue>
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {choices.map((region) => (
                  <SelectItem key={region.key} value={region.key} label={region.name}>{region.name}</SelectItem>
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
