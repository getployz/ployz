import type { ReactNode } from "react";
import { Field, FieldContent, FieldDescription, FieldTitle } from "#/components/ui/field";

/**
 * A destructive action as a plain settings row: what it does, any further lines about it, and the button that does it.
 * The button carries the danger; the row is never red on red.
 */
export function DangerRow({ title, description, action, children }: {
  title: ReactNode;
  description: ReactNode;
  action: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Field orientation="responsive">
      <FieldContent>
        <FieldTitle>{title}</FieldTitle>
        <FieldDescription>{description}</FieldDescription>
        {children}
      </FieldContent>
      <div className="flex shrink-0 gap-2">{action}</div>
    </Field>
  );
}
