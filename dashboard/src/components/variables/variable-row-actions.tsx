import {
  CheckIcon,
  LockIcon,
  MoreVerticalIcon,
  PencilIcon,
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
import type { VariableMetadataPatch } from "#/components/variables/variable-row-types";

export function VariableRowActions({
  editing,
  exported,
  isSaving,
  isSealed,
  plainValue,
  showMetadata,
  onCancelEdit,
  onOpenDeleteDialog,
  onOpenEdit,
  onOpenSealDialog,
  onSave,
  onUpdateMetadata,
}: {
  editing: boolean;
  exported: boolean;
  isSaving: boolean;
  isSealed: boolean;
  plainValue: string;
  showMetadata: boolean;
  onCancelEdit: () => void;
  onOpenDeleteDialog: () => void;
  onOpenEdit: (value: string) => void;
  onOpenSealDialog: () => void;
  onSave: () => void;
  onUpdateMetadata: (patch: VariableMetadataPatch) => void;
}) {
  if (editing) {
    return (
      <div className="flex items-center gap-1">
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={onCancelEdit}
          disabled={isSaving}
        >
          <XIcon />
          <span className="sr-only">Cancel</span>
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-sm"
          onClick={onSave}
          disabled={isSaving}
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
          <Button type="button" variant="ghost" size="icon-sm">
            <MoreVerticalIcon />
            <span className="sr-only">Variable actions</span>
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
          {showMetadata ? (
            <DropdownMenuItem
              onClick={() => onUpdateMetadata({ exported: !exported })}
            >
              <Share2Icon />
              {exported ? "Stop exporting" : "Export"}
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onClick={onOpenDeleteDialog}>
            <TrashIcon />
            Delete
          </DropdownMenuItem>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
