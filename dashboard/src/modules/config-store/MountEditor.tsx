import { useEffect, useId, useRef, useState } from "react";
import type { Persistable } from "#/collections/query-collection";
import { Button } from "#/components/ui/button";
import { Field, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import { StoreRefused } from "./store.contract";
import { mountPathError } from "./store-volumes";

export type MountChoice = { id: string; name: string; refusal: string | null; directory: string };
export type MountContext = { resourceId: string; serviceId?: never } | { serviceId: string; resourceId?: never };
export type MountEditing = { counterpart: string; directory: string; editing: boolean };

export const mountFailure = (error: Error) => error instanceof StoreRefused ? error.message : "Couldn't save the mount. Try again.";

/** Persistence completes only this form; the writer still applies changes immediately. */
export function MountEditor({ label, choices, initial, submit, onClose }: {
  label: "Service" | "Config" | "Volume"; choices: readonly MountChoice[]; initial: MountEditing;
  submit: (counterpart: string, directory: string) => Persistable | null; onClose: () => void;
}) {
  const directoryId = useId();
  const [selected, setSelected] = useState(choices.find((choice) => choice.id === initial.counterpart) ?? null);
  const [directory, setDirectory] = useState(initial.directory);
  const [status, setStatus] = useState<{ kind: "idle"; error: string | null } | { kind: "saving" }>({ kind: "idle", error: null });
  const attempt = useRef(0);
  useEffect(() => () => { attempt.current++; }, []);
  const pending = status.kind === "saving";
  // An optimistic Add filters its choice out before persistence completes.
  const shown = selected && !choices.some((choice) => choice.id === selected.id) ? [...choices, selected] : choices;

  function save() {
    if (pending) return;
    const error = !selected ? `Select a ${label.toLowerCase()}.` : mountPathError(directory);
    if (error) return setStatus({ kind: "idle", error });
    const identity = ++attempt.current;
    try {
      const transaction = submit(selected?.id ?? "", directory);
      if (!transaction) return onClose();
      setStatus({ kind: "saving" });
      transaction.isPersisted.promise.then(() => {
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
      <Field>
        <FieldLabel>{label}</FieldLabel>
        <Select items={shown.map((choice) => ({ value: choice.id, label: choice.name }))} value={selected?.id ?? ""} disabled={pending || initial.editing} onValueChange={(id) => {
          const choice = choices.find((one) => one.id === id) ?? null;
          if (directory === (selected?.directory ?? initial.directory)) setDirectory(choice?.directory ?? initial.directory);
          setSelected(choice); setStatus({ kind: "idle", error: null });
        }}>
          <SelectTrigger aria-label={label} autoFocus={!initial.editing}><SelectValue placeholder={`Select a ${label.toLowerCase()}`} /></SelectTrigger>
          <SelectContent><SelectGroup>{shown.map((choice) => (
            <SelectItem key={choice.id} value={choice.id} disabled={choice.refusal !== null} label={choice.name}>
              {choice.name}{choice.refusal ? ` · ${choice.refusal}` : ""}
            </SelectItem>
          ))}</SelectGroup></SelectContent>
        </Select>
      </Field>
      <Field data-invalid={status.kind === "idle" && status.error ? true : undefined}>
        <FieldLabel htmlFor={directoryId}>Directory</FieldLabel>
        <Input id={directoryId} className="font-mono" value={directory} disabled={pending} autoFocus={initial.editing}
          onChange={(event) => { setDirectory(event.target.value); setStatus({ kind: "idle", error: null }); }} />
        {status.kind === "idle" && status.error ? <FieldError role="alert">{status.error}</FieldError> : null}
      </Field>
      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" onClick={() => { attempt.current++; onClose(); }}>Cancel</Button>
        <Button type="submit" disabled={pending}>{pending ? "Saving…" : initial.editing ? "Save directory" : "Mount"}</Button>
      </div>
    </form>
  );
}
