import { cn } from "#/lib/utils";
import type { ServiceDeploymentDiffKind } from "#/modules/services/service-deployment-diff/fields";

export function ApplyChangeValueCell({
  kind,
  value,
  tone,
}: {
  kind: ServiceDeploymentDiffKind;
  value: string | null;
  tone: "current" | "new";
}) {
  return (
    <div className="min-h-8">
      {value ? (
        <div
          className={cn(
            "flex min-h-8 items-center rounded-lg px-3 font-mono text-sm",
            tone === "current" ? "bg-muted" : null,
            tone === "new" && kind === "remove"
              ? "bg-destructive-soft text-destructive"
              : null,
            tone === "new" && kind === "add"
              ? "bg-success-soft text-success"
              : null,
            tone === "new" && kind !== "add" && kind !== "remove"
              ? "bg-changed-soft text-changed"
              : null,
          )}
        >
          {value}
        </div>
      ) : null}
    </div>
  );
}
