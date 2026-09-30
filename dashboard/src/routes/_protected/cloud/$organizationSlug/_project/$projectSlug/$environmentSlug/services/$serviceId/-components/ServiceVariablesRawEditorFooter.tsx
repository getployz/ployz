import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { DialogFooter } from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";

export function ServiceVariablesRawEditorFooter({
  envText,
  onCancel,
  onSubmit,
  saving,
}: {
  envText: string;
  onCancel: () => void;
  onSubmit: () => void;
  saving: boolean;
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
          disabled={saving}
        >
          {saving ? <Spinner data-icon="inline-start" /> : null}
          Update variables
        </Button>
      </div>
    </DialogFooter>
  );
}
