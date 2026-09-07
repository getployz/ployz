import { PencilIcon, Trash2Icon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Field } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { Item, ItemActions, ItemContent, ItemTitle } from "#/components/ui/item";
import type { VolumeDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/useVolumeDrawerState";

type VolumeMountAttachment = VolumeDrawerState["attachments"][number];

export function VolumeMountItem({
  mount,
  serviceName,
  editError,
  editPath,
  isEditing,
  pending,
  onCancelEdit,
  onDetach,
  onEditPathChange,
  onSaveEdit,
  onStartEdit,
}: {
  mount: VolumeMountAttachment;
  serviceName: string;
  editError: string | null;
  editPath: string;
  isEditing: boolean;
  pending: boolean;
  onCancelEdit: () => void;
  onDetach: () => void;
  onEditPathChange: (value: string) => void;
  onSaveEdit: () => void;
  onStartEdit: () => void;
}) {
  return (
    <Item variant="outline">
      <ItemContent>
        <ItemTitle>{serviceName}</ItemTitle>
        {isEditing ? (
          <Field data-invalid={editError ? true : undefined}>
            <Input
              value={editPath}
              aria-invalid={editError ? true : undefined}
              onChange={(event) => onEditPathChange(event.target.value)}
              autoFocus
            />
            {editError ? (
              <p className="text-sm text-destructive">{editError}</p>
            ) : null}
          </Field>
        ) : (
          <p className="truncate text-sm text-muted-foreground">
            {mount.mountPath}
          </p>
        )}
      </ItemContent>
      <ItemActions>
        {isEditing ? (
          <>
            <Button
              variant="outline"
              size="sm"
              disabled={pending}
              onClick={onCancelEdit}
            >
              Cancel
            </Button>
            <Button size="sm" disabled={pending} onClick={onSaveEdit}>
              Save
            </Button>
          </>
        ) : (
          <>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Edit mount path"
              disabled={pending}
              onClick={onStartEdit}
            >
              <PencilIcon />
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Remove mount"
              disabled={pending}
              onClick={onDetach}
            >
              <Trash2Icon />
            </Button>
          </>
        )}
      </ItemActions>
    </Item>
  );
}
