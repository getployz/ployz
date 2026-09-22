import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";
import { CopyButton } from "#/components/copy-button";
import { AlertTriangleIcon, EyeIcon, EyeOffIcon } from "lucide-react";
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
  unresolvedReferences = [],
  revealed,
  valueTargets,
  onCancelEdit,
  onChangeEditValue,
  onSave,
  onToggleReveal,
}: {
  editing: boolean;
  editValue: string;
  isSaving: boolean;
  isSealed: boolean;
  plainValue: string;
  unresolvedReferences?: readonly string[];
  revealed: boolean;
  valueTargets?: ReferenceTarget[];
  onCancelEdit: () => void;
  onChangeEditValue: (value: string) => void;
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
    <div className="flex min-w-0 flex-1 items-center gap-1.5">
      <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
        {isSealed || !revealed ? MASK : plainValue}
      </span>
      {!isSealed && unresolvedReferences.length > 0 ? (
        <Popover>
          <PopoverTrigger openOnHover render={<Button type="button" variant="ghost" size="icon-sm" aria-label="Unresolved variable reference" />}>
            <AlertTriangleIcon className="text-warning" />
          </PopoverTrigger>
          <PopoverContent>Unknown reference: {unresolvedReferences.join(", ")}</PopoverContent>
        </Popover>
      ) : null}
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
        <CopyButton value={plainValue} label="Copy value" />
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
