import type { NamedRegion } from "@ployz/sdk";
import { useEffect, useState } from "react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { leavingServer, placedServices, unreadEnvironments } from "#/modules/machines/server-move";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import { regionLabels, regionName, regionOf } from "#/modules/machines/server-regions";
import { nameNotSaved, useNameRegion, useNewRegions, type OwnedNamespaces } from "#/modules/machines/server-regions.hooks";
import { serverStatus } from "#/modules/machines/server-status";
import type { RoleGate } from "#/modules/organization/members.queries";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { MoveServerDialog } from "./move-server-dialog";

const NO_REGION = "no region";
const NEW_REGION = "new region";

type Move = { readonly region: string | null; readonly created: NamedRegion | null };

export function ServerRegionSection({ machine, machines, services, owned, regions, organizationSlug, gate }: {
  machine: RuntimeMachineRecord;
  machines: readonly RuntimeMachineRecord[];
  /** What runs on this Server. */
  services: readonly { readonly identity: string; readonly name: string; readonly namespace: string | null }[];
  /** The Environments' Namespaces, which say where `services` run: no region change is told before they are read. */
  owned: OwnedNamespaces;
  /** Undefined until the region names arrive. */
  regions: readonly NamedRegion[] | undefined;
  organizationSlug: string;
  gate: RoleGate;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "region");
  const nameRegion = useNameRegion(organizationSlug);
  const [naming, setNaming] = useState(false);
  const [draft, setDraft] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [moving, setMoving] = useState<Move | null>(null);
  const labels = regionLabels(machines);
  const check = useNewRegions(organizationSlug, naming ? [draft] : [], labels)(draft);
  const choices = regions ?? [];
  const label = policy.region;
  const held = owned.kind === "reading" ? "Reading what runs here…"
    : owned.kind === "failed" ? "Couldn’t read what runs here. Reload to change the region." : null;

  const placed = owned.kind === "read" ? placedServices(services, owned.namespaces) : [];
  const unread = unreadEnvironments(placed);
  const leaving = (region: string | null) => leavingServer(machine.id, region, placed, machines.map((other) => ({
    id: other.id, name: other.name, region: other.region, acceptsServices: other.acceptsServices, offline: serverStatus(other) === "offline",
  })));
  const apply = ({ region, created }: Move) => {
    if (held !== null) return;
    policy.request({ region });
    if (created !== null) nameRegion(created, labels, true).catch((error) => nameNotSaved(created, error));
  };
  const move = (asked: Move) => {
    if (leaving(asked.region).length === 0 && unread.length === 0) apply(asked);
    else setMoving(asked);
  };
  const choose = (value: string | null) => {
    if (value === NEW_REGION) {
      setNaming(true);
      return;
    }
    const region = value === NO_REGION ? null : value;
    if (region !== label) move({ region, created: null });
  };
  const stopNaming = () => {
    setNaming(false);
    setDraft("");
    setConfirming(false);
  };
  const create = check.kind === "new" ? { key: check.key, name: draft.trim() } : null;
  const createIn = (region: NamedRegion) => {
    if (held !== null) {
      setConfirming(false);
      return;
    }
    move({ region: region.key, created: region });
    stopNaming();
  };

  useEffect(() => {
    if (confirming && create !== null) createIn(create);
  }, [confirming, create?.key]);

  return (
    <SettingsSection id="server-region" title="Placement">
      <SettingsRow
        label="Region"
        hint={held ?? (policy.regionQueued ? "Queued" : undefined)}
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
          <Select disabled={!gate.allowed || held !== null} value={label === null ? NO_REGION : regionOf(label, choices)?.key ?? label} onValueChange={choose}>
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
        regionName={moving === null ? null : moving.created?.name ?? regionName(moving.region, regions)}
        rows={moving === null ? [] : leaving(moving.region)}
        unread={unread}
        holdReason={held}
        regions={regions}
        open={moving !== null}
        onOpenChange={(open) => { if (!open) setMoving(null); }}
        onConfirm={() => {
          if (moving !== null) apply(moving);
          setMoving(null);
        }}
      />
    </SettingsSection>
  );
}
