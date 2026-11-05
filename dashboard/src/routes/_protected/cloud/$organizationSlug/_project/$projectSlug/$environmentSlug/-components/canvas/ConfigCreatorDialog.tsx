import { useState } from "react";
import { Button } from "#/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { DnsNameHint } from "#/components/dns-name-hint";
import { dnsName } from "#/utils/dns-name";

/** `onCreate` rejects once its failure has been shown (the Store writer toasts a refusal), so the dialog stays open to try again. */
export function ConfigCreatorDialog({ open, onOpenChange, onCreate }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onCreate: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState("config");
  const [creating, setCreating] = useState(false);
  const converted = dnsName(name);

  function changeOpen(next: boolean) {
    if (creating) return;
    onOpenChange(next);
    if (!next) setName("config");
  }

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!converted.name) return;
    setCreating(true);
    try {
      await onCreate(converted.name);
      onOpenChange(false);
      setName("config");
    } catch {
      // Already shown; the name stays for another try.
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
              <DnsNameHint name={converted} />
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => changeOpen(false)}>Cancel</Button>
            <Button type="submit" disabled={!converted.name || creating}>Create config</Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
