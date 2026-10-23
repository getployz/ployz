import { LockIcon } from "lucide-react";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { warmSealing } from "#/modules/variables/seal.client";

/**
 * While sealing is ready, or a new key is ticked: the sealed variables, which the text never holds and a save never removes, and a Sealed
 * choice for each new key.
 */
export function ServiceVariablesRawEditorSealed({ sealedKeys, newKeys, sealing, problems, onSealingChange }: {
  sealedKeys: readonly string[];
  newKeys: readonly string[];
  /** The new keys ticked Sealed. */
  sealing: readonly string[];
  /** Why a ticked value can't be sealed, by key. */
  problems: ReadonlyMap<string, string>;
  onSealingChange: (key: string, sealed: boolean) => void;
}) {
  if (sealedKeys.length === 0 && newKeys.length === 0) return null;
  return (
    <div className="flex flex-col gap-3">
      {sealedKeys.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {sealedKeys.map((key) => (
            <li key={key} className="flex items-center gap-1.5 text-sm text-muted-foreground">
              <LockIcon className="size-3.5" />
              <span className="font-mono text-xs">{key}</span> · Sealed
            </li>
          ))}
        </ul>
      ) : null}
      {newKeys.length > 0 ? (
        <FieldGroup className="gap-2">
          {newKeys.map((key) => (
            <Field key={key} orientation="horizontal">
              <Checkbox
                id={`raw-sealed-${key}`}
                checked={sealing.includes(key)}
                onCheckedChange={(checked) => {
                  warmSealing();
                  onSealingChange(key, Boolean(checked));
                }}
              />
              <FieldLabel htmlFor={`raw-sealed-${key}`}>
                Seal <span className="font-mono text-xs">{key}</span>
              </FieldLabel>
              {problems.has(key) ? <FieldError>{problems.get(key)}</FieldError> : null}
            </Field>
          ))}
        </FieldGroup>
      ) : null}
    </div>
  );
}
