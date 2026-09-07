import { LayersIcon, LockIcon } from "lucide-react";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";

export function ServiceVariablesRawEditorAlerts({
  duplicateKeys,
  parseError,
  sealedCount,
  submitError,
}: {
  duplicateKeys: string[];
  parseError: string | null;
  sealedCount: number;
  submitError: string | null;
}) {
  const shownDuplicateKeys = duplicateKeys.slice(0, 6);
  const extraDuplicateCount = duplicateKeys.length - shownDuplicateKeys.length;

  return (
    <>
      {sealedCount > 0 ? (
        <Alert>
          <LockIcon />
          <AlertDescription>
            {sealedCount} sealed variable{sealedCount === 1 ? "" : "s"} omitted
            from the editor. The value{sealedCount === 1 ? " is" : "s are"}{" "}
            retained as-is.
          </AlertDescription>
        </Alert>
      ) : null}

      {parseError ? (
        <Alert variant="destructive">
          <AlertTitle>Couldn’t parse</AlertTitle>
          <AlertDescription>{parseError}</AlertDescription>
        </Alert>
      ) : null}

      {submitError ? (
        <Alert variant="destructive">
          <AlertTitle>Couldn’t update variables</AlertTitle>
          <AlertDescription>{submitError}</AlertDescription>
        </Alert>
      ) : null}

      {duplicateKeys.length > 0 && !parseError && !submitError ? (
        <Alert>
          <LayersIcon />
          <AlertDescription>
            {duplicateKeys.length === 1
              ? "Duplicate key will be merged, keeping the last value: "
              : `${duplicateKeys.length} duplicate keys will be merged, keeping the last value: `}
            <span className="font-mono">{shownDuplicateKeys.join(", ")}</span>
            {extraDuplicateCount > 0 ? ` +${extraDuplicateCount} more` : null}
          </AlertDescription>
        </Alert>
      ) : null}
    </>
  );
}
