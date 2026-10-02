import {
  CheckIcon,
  LockIcon,
  MoreVerticalIcon,
  PencilIcon,
  PinIcon,
  PinOffIcon,
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
  isSealed,
  plainValue,
  showMetadata,
  onCancelEdit,
  onDelete,
  onOpenEdit,
  onOpenSealDialog,
  onSave,
  onUpdateMetadata,
  neverSynced,
  onToggleNeverSync,
}: {
  editing: boolean;
  exported: boolean;
  isSealed: boolean;
  plainValue: string;
  showMetadata: boolean;
  onCancelEdit: () => void;
  /** Stages the deletion; Discard undoes it. */
  onDelete: () => void;
  onOpenEdit: (value: string) => void;
  onOpenSealDialog: () => void;
  onSave: () => void;
  onUpdateMetadata: (patch: VariableMetadataPatch) => void;
  /** Whether it is marked Never sync; null offers no Never sync item. */
  neverSynced: boolean | null;
  onToggleNeverSync: () => void;
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
          {neverSynced !== null ? (
            <DropdownMenuItem onClick={onToggleNeverSync}>
              {neverSynced ? <PinOffIcon /> : <PinIcon />}
              {neverSynced ? "Sync again" : "Never sync"}
            </DropdownMenuItem>
          ) : null}
          <DropdownMenuItem onClick={onDelete}>
            <TrashIcon />
            Delete
          </DropdownMenuItem>
        </DropdownMenuGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
