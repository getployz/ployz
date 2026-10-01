import { CopyButton } from "#/components/copy-button";
import { Button } from "#/components/ui/button";
import { DialogFooter } from "#/components/ui/dialog";
import type { RawEditorMode } from "./ServiceVariablesRawEditorTypes";

export function ServiceVariablesRawEditorFooter({
  text,
  mode,
  onCancel,
  onSubmit,
}: {
  text: string;
  mode: RawEditorMode;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <DialogFooter className="min-w-0 sm:flex-wrap sm:justify-between">
      <CopyButton value={text} label={`Copy ${mode.toUpperCase()}`} showLabel variant="outline" />
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
