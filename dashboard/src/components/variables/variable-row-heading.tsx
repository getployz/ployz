import { AlertTriangleIcon } from "lucide-react";

export function VariableRowHeading({
  variableKey,
  exported,
  showMetadata,
  warnings,
}: {
  variableKey: string;
  exported: boolean;
  showMetadata: boolean;
  warnings: Array<string | null | undefined>;
}) {
  return (
    <div className="min-w-0">
      <div className="flex min-w-0 items-center gap-2">
        <span className="truncate font-mono text-sm" title={variableKey}>{variableKey}</span>
        {showMetadata && exported ? (
          <span className="shrink-0 text-xs text-muted-foreground">Exported</span>
        ) : null}
      </div>
      {warnings.map((message) =>
        message ? (
          <div
            key={message}
            className="mt-1 flex items-center gap-1 text-xs text-destructive"
          >
            <AlertTriangleIcon className="size-3" />
            <span className="min-w-0">{message}</span>
          </div>
        ) : null,
      )}
    </div>
  );
}
