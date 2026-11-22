import { useEffect, useId, useRef, useState } from "react";
import { Button } from "#/components/ui/button";
import { SettingsFormField } from "#/components/settings/settings-list";
import { FieldError } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import type { Write } from "#/modules/config-store/store-write";
import { StoreRefused } from "#/lib/refusal";
import { mountPathError } from "#/modules/config-store/store-volumes";

/** A refused choice can still be picked, to read in full why under the field: its option has room only for a few words. */
export type MountChoice = { id: string; name: string; refusal: { short: string; why: string } | null; directory: string };
export type MountEditing = { counterpart: string; directory: string; editing: boolean };

export const mountFailure = (error: Error) => error instanceof StoreRefused ? error.message : "Couldn't save the mount. Try again.";

/** Persistence completes only this form; the writer still applies changes immediately. */
export function MountEditor({ choices, initial, submit, onClose }: {
  choices: readonly MountChoice[]; initial: MountEditing;
  submit: (counterpart: string, directory: string) => Write | null; onClose: () => void;
}) {
  const serviceId = useId();
  const directoryId = useId();
  const [selected, setSelected] = useState(choices.find((choice) => choice.id === initial.counterpart) ?? null);
  const [directory, setDirectory] = useState(initial.directory);
  const [status, setStatus] = useState<{ kind: "idle"; error: string | null } | { kind: "saving" }>({ kind: "idle", error: null });
  const attempt = useRef(0);
  useEffect(() => () => { attempt.current++; }, []);
  const pending = status.kind === "saving";
  // An optimistic Add filters its choice out before persistence completes.
  const current = choices.find((choice) => choice.id === selected?.id) ?? selected;
  const shown = current && !choices.includes(current) ? [...choices, current] : choices;
  const refused = initial.editing ? null : current?.refusal?.why ?? null;

  function save() {
    if (pending || refused) return;
    const error = !selected ? "Select a service." : mountPathError(directory);
    if (error) return setStatus({ kind: "idle", error });
    const identity = ++attempt.current;
    try {
      const transaction = submit(selected?.id ?? "", directory);
      if (!transaction) return onClose();
      setStatus({ kind: "saving" });
      transaction.then(() => {
        if (attempt.current === identity) onClose();
      }, (error: Error) => {
        if (attempt.current === identity) setStatus({ kind: "idle", error: mountFailure(error) });
      });
    } catch (error) {
      setStatus({ kind: "idle", error: error instanceof Error ? error.message : "Couldn't save the mount. Try again." });
    }
  }

  return (
    <form className="flex flex-col gap-3" onSubmit={(event) => { event.preventDefault(); save(); }}
      onKeyDown={(event) => {
        if (event.key !== "Escape" || event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
        if (!(event.target instanceof Node) || !event.currentTarget.contains(event.target)) return;
        event.preventDefault(); event.stopPropagation(); attempt.current++; onClose();
      }}>
      <SettingsFormField label="Service" htmlFor={serviceId}>
        <Select items={shown.map((choice) => ({ value: choice.id, label: choice.name }))} value={selected?.id ?? ""} disabled={pending || initial.editing} onValueChange={(id) => {
          const choice = choices.find((one) => one.id === id) ?? null;
          if (directory === (selected?.directory ?? initial.directory)) setDirectory(choice?.directory ?? initial.directory);
          setSelected(choice); setStatus({ kind: "idle", error: null });
        }}>
          <SelectTrigger id={serviceId} className="w-full" autoFocus={!initial.editing}><SelectValue placeholder="Select a service" /></SelectTrigger>
          <SelectContent><SelectGroup>{shown.map((choice) => (
            <SelectItem key={choice.id} value={choice.id} label={choice.name}>
              {choice.name}{choice.refusal ? ` · ${choice.refusal.short}` : ""}
            </SelectItem>
          ))}</SelectGroup></SelectContent>
        </Select>
        {refused ? <FieldError role="alert">{refused}</FieldError> : null}
      </SettingsFormField>
      <SettingsFormField label="Directory" htmlFor={directoryId}>
        <Input id={directoryId} className="font-mono" value={directory} disabled={pending} autoFocus={initial.editing}
          aria-invalid={status.kind === "idle" && status.error ? true : undefined}
          onChange={(event) => { setDirectory(event.target.value); setStatus({ kind: "idle", error: null }); }} />
        {status.kind === "idle" && status.error ? <FieldError role="alert">{status.error}</FieldError> : null}
      </SettingsFormField>
      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" onClick={() => { attempt.current++; onClose(); }}>Cancel</Button>
        <Button type="submit" disabled={pending || refused !== null}>{pending ? "Saving…" : initial.editing ? "Save directory" : "Mount"}</Button>
      </div>
    </form>
  );
}
