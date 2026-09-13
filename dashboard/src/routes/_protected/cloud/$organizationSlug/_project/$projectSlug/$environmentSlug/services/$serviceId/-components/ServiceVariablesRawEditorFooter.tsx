import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { DialogFooter } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";

export function ServiceVariablesRawEditorFooter({
  copied,
  isSubmitting,
  onCancel,
  onCopyEnv,
  onSubmit,
}: {
  copied: boolean;
  isSubmitting: boolean;
  onCancel: () => void;
  onCopyEnv: () => void;
  onSubmit: () => void;
}) {
  return (
    <DialogFooter className="min-w-0 sm:flex-wrap sm:justify-between">
      <Button
        type="button"
        variant="ghost"
        onClick={onCopyEnv}
      >
        {copied ? (
          <CheckIcon data-icon="inline-start" />
        ) : (
          <CopyIcon data-icon="inline-start" />
        )}
        {copied ? "Copied" : "Copy ENV"}
      </Button>
      <div className="flex min-w-0 flex-wrap items-center justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          onClick={onCancel}
          disabled={isSubmitting}
        >
          Cancel
        </Button>
        <Button
          type="button"
          onClick={onSubmit}
          disabled={isSubmitting}
        >
          {isSubmitting ? <Spinner /> : null}
          {isSubmitting ? "Updating…" : "Update variables"}
        </Button>
      </div>
    </DialogFooter>
  );
}
