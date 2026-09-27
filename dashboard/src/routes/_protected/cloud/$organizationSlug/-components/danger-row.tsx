import type { ReactNode } from "react";

/** A destructive action set apart: what it does, and the button that does it. */
export function DangerRow({ title, description, action }: { title: ReactNode; description: ReactNode; action: ReactNode }) {
  return (
    <div className="flex flex-col items-start justify-between gap-4 rounded-xl border border-destructive-border bg-destructive-soft p-4 sm:flex-row sm:items-center">
      <div className="min-w-0">
        <div className="text-sm font-semibold text-foreground">{title}</div>
        <p className="mt-1 text-sm text-foreground">{description}</p>
      </div>
      {action}
    </div>
  );
}
