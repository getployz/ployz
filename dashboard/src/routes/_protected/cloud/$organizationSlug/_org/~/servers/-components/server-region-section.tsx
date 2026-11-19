import type { NamedRegion, OwnedNamespace } from "@ployz/sdk";
import { useEffect, useState } from "react";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectSeparator, SelectTrigger, SelectValue } from "#/components/ui/select";
import { leavingServer, placedServices, unreadEnvironments } from "#/modules/machines/server-move";
import { useServerPolicy } from "#/modules/machines/server-policy.hooks";
import { regionLabels, regionName, regionOf } from "#/modules/machines/server-regions";
import { nameNotSaved, useNameRegion, useNewRegions, useOwnedNamespaces } from "#/modules/machines/server-regions.hooks";
import { needsAttention, serverStatus } from "#/modules/machines/server-status";
import type { RoleGate } from "#/modules/organization/members.queries";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { MoveServerDialog } from "./move-server-dialog";

const NO_REGION = "no region";
const NEW_REGION = "new region";

type Move = { readonly region: string | null; readonly created: NamedRegion | null };

export function ServerRegionSection({ machine, machines, services, regions, organizationSlug, gate }: {
  machine: RuntimeMachineRecord;
  machines: readonly RuntimeMachineRecord[];
  /** What runs on this Server. */
  services: readonly { readonly identity: string; readonly name: string; readonly namespace: string | null }[];
  /** Undefined until the region names arrive. */
  regions: readonly NamedRegion[] | undefined;
  organizationSlug: string;
  gate: RoleGate;
}) {
  const policy = useServerPolicy(machine, organizationSlug, "region");
  const nameRegion = useNameRegion(organizationSlug);
  const owned = useOwnedNamespaces(organizationSlug);
  const [naming, setNaming] = useState(false);
  const [draft, setDraft] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [moving, setMoving] = useState<Move | null>(null);
  const [waiting, setWaiting] = useState<Move | null>(null);
  const labels = regionLabels(machines);
  const check = useNewRegions(organizationSlug, naming ? [draft] : [], labels)(draft);
  const choices = regions ?? [];
  const label = policy.region;
  const held = owned.shown.kind === "reading" ? "Reading what runs here…"
    : owned.shown.kind === "failed" ? "Couldn’t read what runs here. Reload to change the region." : null;
  const shown = owned.shown.kind === "read" ? owned.shown.namespaces : null;

  const servers = machines.map((other) => ({
    id: other.id, name: other.name, region: other.region, acceptsServices: other.acceptsServices, reachable: !needsAttention(serverStatus(other)),
  }));
  const leaving = (namespaces: readonly OwnedNamespace[], region: string | null) => {
    const placed = placedServices(services, namespaces);
    return { rows: leavingServer(machine.id, region, placed, servers), unread: unreadEnvironments(placed) };
  };
  /** What runs here as the read holds it when called, which a render can be behind. Null unless that answer is current. */
  const current = () => {
    const now = owned.now();
    return now.kind === "read" ? now.namespaces : null;
  };
  const send = ({ region, created }: Move) => {
    policy.request({ region });
    if (created !== null) nameRegion(created, labels, true).catch((error) => nameNotSaved(created, error));
  };
  const change = (move: Move) => {
    const namespaces = current();
    if (namespaces === null) return false;
    const told = leaving(namespaces, move.region);
    if (told.rows.length + told.unread.length > 0) setMoving(move);
    else send(move);
    return true;
  };
  const confirmMove = () => {
    const namespaces = current();
    if (moving === null || namespaces === null || shown === null) return;
    if (JSON.stringify(leaving(namespaces, moving.region)) !== JSON.stringify(leaving(shown, moving.region))) return;
    send(moving);
    setMoving(null);
  };
  const choose = (value: string | null) => {
    if (value === NEW_REGION) {
      setNaming(true);
      return;
    }
    const region = value === NO_REGION ? null : value;
    const move = { region, created: null };
    if (region !== label && !change(move)) setWaiting(move);
  };
  const stopNaming = () => {
    setNaming(false);
    setDraft("");
    setConfirming(false);
  };
  const create = check.kind === "new" ? { key: check.key, name: draft.trim() } : null;
  const createIn = (region: NamedRegion) => {
    if (change({ region: region.key, created: region })) stopNaming();
    else setConfirming(false);
  };
  const told = moving === null || shown === null ? null : leaving(shown, moving.region);

  useEffect(() => {
    if (waiting === null) return;
    if (owned.shown.kind === "failed") setWaiting(null);
    else if (owned.shown.kind === "read" && change(waiting)) setWaiting(null);
  }, [owned.shown, waiting]);
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
        rows={told?.rows ?? []}
        unread={told?.unread ?? []}
        holdReason={held}
        regions={regions}
        open={moving !== null}
        onOpenChange={(open) => { if (!open) setMoving(null); }}
        onConfirm={confirmMove}
      />
    </SettingsSection>
  );
}
