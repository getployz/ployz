import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { DialogFooter } from "#/components/ui/dialog";

export function ServiceVariablesRawEditorFooter({
  envText,
  onCancel,
  onSubmit,
}: {
  envText: string;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <DialogFooter className="min-w-0 sm:flex-wrap sm:justify-between">
      <CopyButton value={envText} label="Copy ENV" showLabel variant="outline" />
      <div className="flex min-w-0 flex-wrap items-center justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          onClick={onCancel}
        >
          Cancel
        </Button>
        <Button
          type="button"
          onClick={onSubmit}
        >
          Update variables
        </Button>
      </div>
    </DialogFooter>
  );
}
