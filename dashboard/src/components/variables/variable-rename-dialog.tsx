import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { variableKeyError } from "#/modules/variables/variables";

/** Renames a variable; its value, sealed or not, moves with it and is never read here. */
export function VariableRenameDialog({ variableKey, open, onOpenChange, onRename }: {
  variableKey: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Saves in the background; the row shows the new name at once. */
  onRename: (to: string) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Rename</DialogTitle>
        </DialogHeader>
        <RenameForm variableKey={variableKey} onRename={(to) => { onRename(to); onOpenChange(false); }} />
      </DialogContent>
    </Dialog>
  );
}

function RenameForm({ variableKey, onRename }: { variableKey: string; onRename: (to: string) => void }) {
  const [typed, setTyped] = useState(variableKey);
  const to = typed.trim().toUpperCase();
  const error = to ? variableKeyError(to) : null;
  return (
    <form className="flex flex-col gap-4" onSubmit={(event) => {
      event.preventDefault();
      if (to && !error && to !== variableKey) onRename(to);
    }}>
      <Field>
        <FieldLabel htmlFor="variable-rename">Key</FieldLabel>
        <Input id="variable-rename" autoFocus value={typed} onChange={(event) => setTyped(event.target.value)}
          className="font-mono text-xs uppercase" aria-invalid={error ? true : undefined} />
        {error ? <FieldError>{error}</FieldError> : null}
      </Field>
      <Button type="submit" className="self-start" disabled={!to || error !== null || to === variableKey}>Save</Button>
    </form>
  );
}
