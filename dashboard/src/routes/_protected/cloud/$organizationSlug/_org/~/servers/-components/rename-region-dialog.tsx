import type { NamedRegion } from "@ployz/sdk";
import { MapPinPenIcon } from "lucide-react";
import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { DEFAULT_REGION, regionNameProblem } from "#/modules/machines/server-regions";
import { StoreRefused } from "#/lib/refusal";
import { useNameRegion } from "#/modules/machines/server-regions.hooks";
import type { RoleGate } from "#/modules/organization/members.queries";

/** Only the name changes; the key stays on every Server, so nothing restarts. */
export function RenameRegionDialog({ organizationSlug, regions, labels, gate }: {
  organizationSlug: string;
  /** The Servers' `region` labels, which the Store checks the region against. */
  labels: readonly string[];
  regions: readonly NamedRegion[];
  gate: RoleGate;
}) {
  const nameRegion = useNameRegion(organizationSlug);
  const [open, setOpen] = useState(false);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const region = regions.find((each) => each.key === key);
  const pick = (next: NamedRegion | undefined) => {
    setKey(next?.key ?? "");
    setName(next?.name ?? "");
    setRefused(null);
  };
  const [saving, setSaving] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const renamed = name.trim();
  const problem = renamed === "" ? null : regionNameProblem(renamed);
  const rename = async () => {
    if (region === undefined || renamed === "" || renamed === region.name || problem !== null || saving) return;
    setSaving(true);
    setRefused(null);
    try {
      await nameRegion({ key: region.key, name: renamed }, labels);
      setOpen(false);
    } catch (error) {
      // The dialog stays with what was typed; a refusal the Store gives for the name shows beside it.
      setRefused(error instanceof StoreRefused ? error.message : null);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <Button variant="ghost" disabled={!gate.allowed} title={gate.reason ?? "Rename region"} onClick={() => {
        pick(regions.find((each) => each.key === DEFAULT_REGION) ?? regions[0]);
        setOpen(true);
      }}>
        <MapPinPenIcon data-icon="inline-start" className="sm:hidden" />
        <span className="max-sm:sr-only">Rename region</span>
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Rename region</DialogTitle>
          </DialogHeader>
          <form id="rename-region" onSubmit={(event) => { event.preventDefault(); void rename(); }}>
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="rename-region-key">Region</FieldLabel>
                <NativeSelect id="rename-region-key" value={key} onChange={(event) => pick(regions.find((each) => each.key === event.target.value))}>
                  {regions.map((each) => <NativeSelectOption key={each.key} value={each.key}>{each.name}</NativeSelectOption>)}
                </NativeSelect>
              </Field>
              <Field data-invalid={problem !== null || refused !== null || undefined}>
                <FieldLabel htmlFor="rename-region-name">Name</FieldLabel>
                <Input id="rename-region-name" autoFocus value={name} aria-invalid={problem !== null || refused !== null || undefined}
                  onChange={(event) => { setName(event.target.value); setRefused(null); }} />
                {problem ?? refused ? <FieldError>{problem ?? refused}</FieldError> : <FieldDescription>Nothing restarts.</FieldDescription>}
                <FieldDescription className="font-mono">--region {key}</FieldDescription>
              </Field>
            </FieldGroup>
          </form>
          <DialogFooter>
            <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" form="rename-region" disabled={region === undefined || renamed === "" || renamed === region.name || problem !== null || saving}>Rename region</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
