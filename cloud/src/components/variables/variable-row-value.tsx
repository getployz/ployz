import { CopyIcon, EyeIcon, EyeOffIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "#/components/ui/tooltip";
import { VariableValueInput } from "#/components/variables/VariableValueInput";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";

const MASK = "*******";

export function VariableRowValue({
  editing,
  editValue,
  isSaving,
  isSealed,
  plainValue,
  revealed,
  valueTargets,
  onCancelEdit,
  onChangeEditValue,
  onCopy,
  onSave,
  onToggleReveal,
}: {
  editing: boolean;
  editValue: string;
  isSaving: boolean;
  isSealed: boolean;
  plainValue: string;
  revealed: boolean;
  valueTargets?: ReferenceTarget[];
  onCancelEdit: () => void;
  onChangeEditValue: (value: string) => void;
  onCopy: () => void;
  onSave: () => void;
  onToggleReveal: () => void;
}) {
  if (editing) {
    return (
      <VariableValueInput
        autoFocus
        value={editValue}
        onValueChange={onChangeEditValue}
        targets={valueTargets ?? []}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            event.preventDefault();
            onSave();
          }
          if (event.key === "Escape") {
            event.preventDefault();
            onCancelEdit();
          }
        }}
        className="font-mono text-xs"
        disabled={isSaving}
      />
    );
  }

  return (
    <div className="flex min-w-0 items-center gap-1.5">
      <span className="truncate font-mono text-xs text-muted-foreground">
        {isSealed || !revealed ? MASK : plainValue}
      </span>
      {!isSealed ? (
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={onToggleReveal}
        >
          {revealed ? <EyeOffIcon /> : <EyeIcon />}
          <span className="sr-only">
            {revealed ? "Hide value" : "Show value"}
          </span>
        </Button>
      ) : null}
      {!isSealed ? (
        <Button type="button" variant="ghost" size="icon-sm" onClick={onCopy}>
          <CopyIcon />
          <span className="sr-only">Copy value</span>
        </Button>
      ) : null}
      {isSealed ? (
        <Tooltip>
          <TooltipTrigger
            render={
              <span className="inline-flex size-4 items-center justify-center rounded-full bg-muted text-[10px] font-medium text-muted-foreground">
                !
              </span>
            }
          />
          <TooltipContent>Sealed values cannot be revealed.</TooltipContent>
        </Tooltip>
      ) : null}
    </div>
  );
}
