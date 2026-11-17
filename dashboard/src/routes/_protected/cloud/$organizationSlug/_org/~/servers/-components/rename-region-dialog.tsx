import type { Region } from "@ployz/sdk";
import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { useNameRegion } from "#/modules/machines/server-regions.hooks";
import type { RoleGate } from "#/modules/organization/members.queries";

/** Renames one of `regions`. Only its name changes: its key stays on every Server, so nothing restarts. */
export function RenameRegionDialog({ organizationSlug, regions, gate }: {
  organizationSlug: string;
  regions: readonly Region[];
  gate: RoleGate;
}) {
  const nameRegion = useNameRegion(organizationSlug);
  const [open, setOpen] = useState(false);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const region = regions.find((each) => each.key === key);
  const pick = (next: Region | undefined) => {
    setKey(next?.key ?? "");
    setName(next?.name ?? "");
  };
  const renamed = name.trim();
  const rename = () => {
    if (region === undefined || renamed === "" || renamed === region.name) return;
    void nameRegion({ key: region.key, name: renamed });
    setOpen(false);
  };

  return (
    <>
      <Button variant="ghost" disabled={!gate.allowed} title={gate.reason} onClick={() => {
        pick(regions.find((each) => each.key === "default") ?? regions[0]);
        setOpen(true);
      }}>
        Rename region
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Rename region</DialogTitle>
          </DialogHeader>
          <form id="rename-region" onSubmit={(event) => { event.preventDefault(); rename(); }}>
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="rename-region-key">Region</FieldLabel>
                <NativeSelect id="rename-region-key" value={key} onChange={(event) => pick(regions.find((each) => each.key === event.target.value))}>
                  {regions.map((each) => <NativeSelectOption key={each.key} value={each.key}>{each.name}</NativeSelectOption>)}
                </NativeSelect>
              </Field>
              <Field>
                <FieldLabel htmlFor="rename-region-name">Name</FieldLabel>
                <Input id="rename-region-name" autoFocus value={name} onChange={(event) => setName(event.target.value)} />
                <FieldDescription>Nothing restarts.</FieldDescription>
                <FieldDescription className="font-mono">--region {key}</FieldDescription>
              </Field>
            </FieldGroup>
          </form>
          <DialogFooter>
            <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" form="rename-region" disabled={region === undefined || renamed === "" || renamed === region.name}>Rename region</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
