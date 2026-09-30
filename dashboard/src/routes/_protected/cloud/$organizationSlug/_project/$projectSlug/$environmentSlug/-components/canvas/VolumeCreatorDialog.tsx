import { useState } from "react";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "#/components/ui/dialog";
import {
  Field,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import type { VolumeKind } from "@ployz/sdk";
import { VolumeStorageFields } from "#/modules/config-store/VolumeStorageFields";
import { DEFAULT_VOLUME_GB, volumeStorage } from "#/modules/config-store/store-volumes";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { FlowPosition } from "./types";

export function VolumeCreatorDialog({
  organizationSlug,
  open,
  onOpenChange,
  position,
  onCreate,
}: {
  organizationSlug: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  position: FlowPosition;
  onCreate: (input: {
    name: string;
    storage: VolumeKind;
    position: FlowPosition;
  }) => void;
}) {
  const [name, setName] = useState("data");
  const [managed, setManaged] = useState(true);
  const [sizeGB, setSizeGB] = useState(DEFAULT_VOLUME_GB);
  const trimmedName = name.trim();
  const runtime = useRuntimeLens(organizationSlug);
  const needsServer = runtime.noServers || (runtime.status === "observed" && !runtime.incomplete
    && runtime.machines.every((machine) => machine.storage !== null)
    && !runtime.machines.some((machine) => machine.acceptsServices && ["up", "suspect"].includes(machine.membership)
      && (machine.storage === "ready" || machine.storage === "pool")));

  function changeOpen(open: boolean) {
    onOpenChange(open);
    if (!open) {
      setName("data");
      setManaged(true);
      setSizeGB(DEFAULT_VOLUME_GB);
    }
  }

  // The Volume shows at once and saves in the background; a refused name comes back as a toast.
  function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const storage = volumeStorage(managed, sizeGB);
    if (!trimmedName || !storage) return;
    onCreate({ name: trimmedName, storage, position });
    changeOpen(false);
  }

  return (
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent>
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>Create volume</DialogTitle>
            <DialogDescription>Files stored here survive deployments and restarts.</DialogDescription>
          </DialogHeader>
          <FieldGroup className="py-4">
            <Field>
              <FieldLabel htmlFor="volume-name">Name</FieldLabel>
              <Input
                id="volume-name"
                value={name}
                onChange={(event) => setName(event.target.value)}
                autoComplete="off"
                autoFocus
              />
            </Field>
            <VolumeStorageFields managed={managed} sizeGB={sizeGB} onManagedChange={setManaged} onSizeChange={setSizeGB} />
            {managed && needsServer ? <p className="text-sm text-muted-foreground">Managed volumes need a compatible server before deployment.</p> : null}
          </FieldGroup>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => changeOpen(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!trimmedName || !volumeStorage(managed, sizeGB)}>Create volume</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
