import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { variableKeyError } from "#/modules/variables/variables";

/** Renames a variable; its value, sealed or not, moves with it and is never read here. */
export function VariableRenameDialog({ variableKey, referrers = [], open, onOpenChange, onRename }: {
  variableKey: string;
  /** The variables that reference it, as `SERVICE.KEY`: they switch to the new name with it. */
  referrers?: readonly string[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Saves in the background; the row shows the new name at once. */
  onRename: (to: string, updateReferences: boolean) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Rename</DialogTitle>
        </DialogHeader>
        <RenameForm variableKey={variableKey} referrers={referrers}
          onRename={(to) => { onRename(to, referrers.length > 0); onOpenChange(false); }} />
      </DialogContent>
    </Dialog>
  );
}

function RenameForm({ variableKey, referrers, onRename }: {
  variableKey: string;
  referrers: readonly string[];
  onRename: (to: string) => void;
}) {
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
      {referrers.length > 0 ? (
        <div className="flex flex-col gap-2">
          <FieldDescription>
            {referrers.length === 1 ? "1 variable uses it. It switches" : `${referrers.length} variables use it. They switch`} to the new name.
          </FieldDescription>
          <ul aria-label="Variables that use it" className="flex flex-col gap-1">
            {referrers.map((referrer) => <li key={referrer} className="font-mono text-xs">{referrer}</li>)}
          </ul>
        </div>
      ) : null}
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" disabled={!to || error !== null || to === variableKey}>Rename</Button>
      </DialogFooter>
    </form>
  );
}
