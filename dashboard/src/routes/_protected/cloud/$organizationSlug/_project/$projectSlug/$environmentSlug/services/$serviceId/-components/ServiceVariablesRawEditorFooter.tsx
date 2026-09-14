import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { DialogFooter } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";

export function ServiceVariablesRawEditorFooter({
  envText,
  isSubmitting,
  onCancel,
  onSubmit,
}: {
  envText: string;
  isSubmitting: boolean;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <DialogFooter className="min-w-0 sm:flex-wrap sm:justify-between">
      <CopyButton value={envText} label="Copy ENV" showLabel variant="outline" disabled={isSubmitting} />
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
