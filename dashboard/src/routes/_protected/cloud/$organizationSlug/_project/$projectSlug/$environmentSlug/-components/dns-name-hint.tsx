import { FieldError } from "#/components/ui/field";
import type { DnsName } from "#/utils/dns-name";

/** The line under a name field: the rule when nothing survives conversion, else the saved name when it differs. */
export function DnsNameHint({ name }: { name: DnsName }) {
  if (name.error) return <FieldError>{name.error}</FieldError>;
  if (!name.preview) return null;
  return <p className="text-sm text-muted-foreground">Saved as <code className="font-mono">{name.preview}</code>.</p>;
}
