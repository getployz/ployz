import { useState } from "react";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import {
  Field,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import type { FlowPosition } from "./types";

export function VolumeCreatorDialog({
  open,
  onOpenChange,
  position,
  onCreate,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  position: FlowPosition;
  onCreate: (input: {
    name: string;
    position: FlowPosition;
  }) => void;
}) {
  const [name, setName] = useState("data");
  const trimmedName = name.trim();

  // The Volume shows at once and saves in the background; a refused name comes back as a toast.
  function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!trimmedName) return;
    onCreate({ name: trimmedName, position });
    onOpenChange(false);
    setName("data");
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form onSubmit={handleSubmit}>
          <DialogHeader>
            <DialogTitle>Create volume</DialogTitle>
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
          </FieldGroup>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={!trimmedName}>Create volume</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
