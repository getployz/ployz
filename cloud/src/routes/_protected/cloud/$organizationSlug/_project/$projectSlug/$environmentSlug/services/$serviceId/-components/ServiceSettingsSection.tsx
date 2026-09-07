import type { ReactNode } from "react";
import { cn } from "#/lib/utils";

export function ServiceSettingsSection({
  id,
  title,
  variant = "default",
  children,
}: {
  id: string;
  title: string;
  variant?: "default" | "danger";
  children: ReactNode;
}) {
  const isDanger = variant === "danger";

  return (
    <section data-sec={id} className="flex flex-col gap-3">
      <h2
        className={cn("text-lg font-semibold", isDanger && "text-destructive")}
      >
        {title}
      </h2>
      <div>{children}</div>
    </section>
  );
}
