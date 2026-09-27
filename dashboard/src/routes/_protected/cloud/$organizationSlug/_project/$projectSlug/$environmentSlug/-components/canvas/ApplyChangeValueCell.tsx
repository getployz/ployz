import { cn } from "#/lib/utils";
import type { ServiceDeploymentDiffKind } from "#/modules/services/service-deployment-diff/fields";

/** Staged changes are the user's pending intent; applied ones are what a deployment attempt set, shown neutral. */
export type ApplyChangeTone = "staged" | "applied";

export function ApplyChangeValueCell({
  kind,
  value,
  tone,
  side,
}: {
  kind: ServiceDeploymentDiffKind;
  value: string | null;
  tone: ApplyChangeTone;
  side: "current" | "new";
}) {
  const staged = tone === "staged" && side === "new";
  return (
    <div className="min-h-8">
      {value ? (
        <div
          className={cn(
            "flex min-h-8 items-center rounded-lg px-3 font-mono text-sm",
            side === "current" ? "bg-muted" : null,
            side === "current" && tone === "applied" ? "text-muted-foreground line-through" : null,
            tone === "applied" ? "break-all" : null,
            side === "new" && tone === "applied" ? "border" : null,
            staged && kind === "remove"
              ? "bg-destructive-soft text-destructive"
              : null,
            staged && kind === "add"
              ? "bg-success-soft text-success"
              : null,
            staged && kind !== "add" && kind !== "remove"
              ? "bg-changed-soft text-changed-deep"
              : null,
          )}
        >
          {value}
        </div>
      ) : null}
    </div>
  );
}
