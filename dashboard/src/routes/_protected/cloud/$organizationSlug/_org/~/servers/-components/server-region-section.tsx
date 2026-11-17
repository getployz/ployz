import type { Region } from "@ployz/sdk";
import { useEffect, useState } from "react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { leavingServer, type Leaving, type Placed } from "#/modules/machines/server-move";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import { regionLabels, regionName, regionOf } from "#/modules/machines/server-regions";
import { useNameRegion, useNewRegions } from "#/modules/machines/server-regions.hooks";
import type { RoleGate } from "#/modules/organization/members.queries";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { MoveServerDialog } from "./move-server-dialog";

const NO_REGION = "no region";
const NEW_REGION = "new region";

type Move = { readonly region: string | null; readonly name: string | null; readonly created: Region | null; readonly leaving: readonly Leaving[] };

export function ServerRegionSection({ machine, machines, placed, regions, organizationSlug, gate }: {
  machine: RuntimeMachineRecord;
  machines: readonly RuntimeMachineRecord[];
  placed: readonly Placed[];
  /** Undefined until the region names arrive. */
  regions: readonly Region[] | undefined;
  organizationSlug: string;
  gate: RoleGate;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "region");
  const nameRegion = useNameRegion(organizationSlug);
  const [naming, setNaming] = useState(false);
  const [draft, setDraft] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [moving, setMoving] = useState<Move | null>(null);
  const check = useNewRegions(organizationSlug, naming ? [draft] : [], regionLabels(machines))(draft);
  const choices = regions ?? [];
  const label = policy.region;

  const apply = (region: string | null, created: Region | null) => {
    policy.request({ region });
    if (created !== null) void nameRegion(created);
  };
  const move = (region: string | null, created: Region | null = null) => {
    const leaving = leavingServer(machine.id, region, placed, machines);
    if (leaving.length === 0) apply(region, created);
    else setMoving({ region, name: created?.name ?? regionName(region, regions), created, leaving });
  };
  const choose = (value: string | null) => {
    if (value === NEW_REGION) {
      setNaming(true);
      return;
    }
    const region = value === NO_REGION ? null : value;
    if (region !== label) move(region);
  };
  const stopNaming = () => {
    setNaming(false);
    setDraft("");
    setConfirming(false);
  };
  const create = check.kind === "new" ? { key: check.key, name: draft.trim() } : null;
  const createIn = (region: Region) => {
    move(region.key, region);
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
      <MoveServerDialog
        serverName={machine.name}
        regionName={moving?.name ?? null}
        rows={moving?.leaving ?? []}
        regions={regions}
        open={moving !== null}
        onOpenChange={(open) => { if (!open) setMoving(null); }}
        onConfirm={() => {
          if (moving !== null) apply(moving.region, moving.created);
          setMoving(null);
        }}
      />
    </SettingsSection>
  );
}
