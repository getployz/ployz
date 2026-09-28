import { Field, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";

/** The Branch's name, checked as its namespace before anything is sent, and the web addresses it gives the copies. */
export function NameSection({ name, onName, error, addresses }: {
  name: string;
  onName: (name: string) => void;
  error: string | null;
  addresses: string[];
}) {
  return (
    <Field data-invalid={error !== null || undefined}>
      <FieldLabel htmlFor="branch-name">Name</FieldLabel>
      <Input id="branch-name" value={name} onChange={(event) => onName(event.target.value)}
        aria-invalid={error !== null || undefined} autoComplete="off" spellCheck={false} />
      {error ? <FieldError>{error}</FieldError> : addresses.length > 0 && (
        <FieldDescription>
          Web addresses: {addresses.map((address, index) => (
            <span key={address}>{index > 0 && ", "}<code className="font-mono">{address}</code></span>
          ))}
        </FieldDescription>
      )}
    </Field>
  );
}
