import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";
import { CopyButton } from "#/components/copy-button";
import { AlertTriangleIcon, EyeIcon, EyeOffIcon } from "lucide-react";
import type { KeyboardEvent } from "react";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "#/components/ui/tooltip";
import { VariableValueInput } from "#/components/variables/VariableValueInput";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";

const MASK = "*******";

export function VariableRowValue({
  editing,
  editValue,
  isSealed,
  needsValue,
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
  isSealed: boolean;
  /** A secret still without a value: it says so, and its edit sets one. */
  needsValue: boolean;
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
    const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
      if (event.key === "Enter") {
        event.preventDefault();
        onSave();
      }
      if (event.key === "Escape") {
        event.preventDefault();
        onCancelEdit();
      }
    };
    // A secret's value is typed blind and stored as-is.
    return isSealed ? (
      <Input autoFocus type="password" autoComplete="off" aria-label="Secret value" value={editValue}
        onChange={(event) => onChangeEditValue(event.target.value)} onKeyDown={onKeyDown} />
    ) : (
      <VariableValueInput
        autoFocus
        value={editValue}
        onValueChange={onChangeEditValue}
        targets={valueTargets ?? []}
        onKeyDown={onKeyDown}
        className="font-mono text-xs"
      />
    );
  }

  return (
    <div className="flex min-w-0 flex-1 items-center gap-1.5">
      {needsValue ? (
        <span className="min-w-0 flex-1 truncate text-xs text-warning">needs a value</span>
      ) : (
        <span className="ph-no-capture min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
          {isSealed || !revealed ? MASK : plainValue}
        </span>
      )}
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
