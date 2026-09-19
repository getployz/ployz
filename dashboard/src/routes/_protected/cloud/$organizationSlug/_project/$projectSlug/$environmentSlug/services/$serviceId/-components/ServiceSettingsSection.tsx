import type { ReactNode } from "react";
import { cn } from "#/lib/utils";

export function ServiceSettingsSection({
  id,
  title,
  description,
  variant = "default",
  children,
}: {
  id: string;
  title: string;
  description?: string;
  variant?: "default" | "danger";
  children: ReactNode;
}) {
  const isDanger = variant === "danger";

  return (
    <section
      data-sec={id}
      className={cn(
        "overflow-hidden rounded-xl border bg-card",
        isDanger && "border-destructive/50",
      )}
    >
      <div className="flex flex-col gap-1 px-4 pt-4">
        <h2 className={cn("text-base font-medium", isDanger && "text-destructive")}>{title}</h2>
        {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
      </div>
      <div className="p-4 text-sm">{children}</div>
    </section>
  );
}
