import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";

export function ConfigCreatorDialog({ open, onOpenChange, onCreate }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreate: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState("config");
  const [creating, setCreating] = useState(false);
  const trimmedName = name.trim();

  function changeOpen(next: boolean) {
    if (creating) return;
    onOpenChange(next);
    if (!next) setName("config");
  }

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!trimmedName) return;
    setCreating(true);
    try {
      await onCreate(trimmedName);
      onOpenChange(false);
      setName("config");
    } finally {
      setCreating(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={changeOpen}>
      <DialogContent>
        <form onSubmit={(event) => void handleSubmit(event)}>
          <DialogHeader>
            <DialogTitle>Create config</DialogTitle>
            <DialogDescription>Files mounted into services.</DialogDescription>
          </DialogHeader>
          <FieldGroup className="py-4">
            <Field>
              <FieldLabel htmlFor="config-name">Name</FieldLabel>
              <Input id="config-name" value={name} onChange={(event) => setName(event.target.value)} autoComplete="off" autoFocus />
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => changeOpen(false)}>Cancel</Button>
            <Button type="submit" disabled={!trimmedName || creating}>Create config</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
