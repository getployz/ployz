import { AlertTriangleIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";

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
      <div className="truncate font-mono text-sm">{variableKey}</div>
      {showMetadata ? (
        <div className="mt-1 flex flex-wrap gap-1">
          {exported ? <Badge variant="secondary">Exported</Badge> : null}
        </div>
      ) : null}
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
