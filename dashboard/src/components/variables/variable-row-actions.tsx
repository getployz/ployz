import {
  CheckIcon,
  LockIcon,
  MoreVerticalIcon,
  PencilIcon,
  PinIcon,
  PinOffIcon,
  TextCursorInputIcon,
  Share2Icon,
  TrashIcon,
  XIcon,
} from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";

export function VariableRowActions({
  variableKey,
  editing,
  refocus,
  exported,
  isSealed,
  plainValue,
  onCancelEdit,
  onDelete,
  onOpenEdit,
  onOpenSealDialog,
  onSave,
  onToggleExported,
  neverSynced,
  onToggleNeverSync,
  onOpenRename,
}: {
  variableKey: string;
  editing: boolean;
  /** Focus the menu as it comes back after an edit. */
  refocus: boolean;
  exported: boolean;
  isSealed: boolean;
  plainValue: string;
  onCancelEdit: () => void;
  /** Stages the deletion; Discard undoes it. */
  onDelete: () => void;
  onOpenEdit: (value: string) => void;
  onOpenSealDialog: () => void;
  onSave: () => void;
  onToggleExported: () => void;
  /** Whether it is marked Never sync. */
  neverSynced: boolean;
  onToggleNeverSync: () => void;
  onOpenRename: () => void;
}) {
  if (editing) {
    return (
      <div className="flex items-center gap-1">
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={onCancelEdit}
        >
          <XIcon />
          <span className="sr-only">Cancel</span>
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={onSave}
        >
          <CheckIcon />
          <span className="sr-only">Save</span>
        </Button>
      </div>
    );
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={
          <Button type="button" variant="ghost" size="icon-sm" autoFocus={refocus}>
            <MoreVerticalIcon />
            <span className="sr-only">Actions for {variableKey}</span>
          </Button>
        }
      />
      <DropdownMenuContent align="end">
        <DropdownMenuGroup>
          {!isSealed ? (
            <DropdownMenuItem onClick={() => onOpenEdit(plainValue)}>
              <PencilIcon />
              Edit
            </DropdownMenuItem>
          ) : null}
          {!isSealed ? (
            <DropdownMenuItem onClick={onOpenSealDialog}>
              <LockIcon />
              Seal
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onClick={onOpenRename}>
            <TextCursorInputIcon />
            Rename
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onToggleExported}>
            <Share2Icon />
            {exported ? "Stop exporting" : "Export"}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onToggleNeverSync}>
            {neverSynced ? <PinOffIcon /> : <PinIcon />}
            {neverSynced ? "Sync again" : "Never sync"}
          </DropdownMenuItem>
          <DropdownMenuItem onClick={onDelete}>
            <TrashIcon />
            Delete
          </DropdownMenuItem>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
