import type { ReactNode } from "react";

/** A destructive action set apart: what it does, any further lines about it, and the button that does it. */
export function DangerRow({ title, description, action, children }: {
  title: ReactNode;
  description: ReactNode;
  action: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-foreground">{title}</div>
        <p className="mt-1 text-sm text-foreground">{description}</p>
        {children}
      </div>
      {action}
    </div>
  );
}
