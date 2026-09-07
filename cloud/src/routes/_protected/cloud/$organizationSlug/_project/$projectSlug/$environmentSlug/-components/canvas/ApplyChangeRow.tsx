import { Trash2Icon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { TableCell, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import type { CanvasNodeDiffGroup } from "#/modules/environment-design/canvas-node-diff";
import {
  getKindBadgeVariant,
  getKindIcon,
  getKindTextClassName,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/apply-changes-display";
import { ApplyChangeValueCell as ValueCell } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeValueCell";

type CanvasNodeDiffRow = CanvasNodeDiffGroup["rows"][number];

export function ApplyChangeRow({
  group,
  row,
  totalChanges,
  showCurrentValue,
  showNewValue,
  onCloseDialog,
  onDiscardRow,
}: {
  group: CanvasNodeDiffGroup;
  row: CanvasNodeDiffRow;
  totalChanges: number;
  showCurrentValue: boolean;
  showNewValue: boolean;
  onCloseDialog: () => void;
  onDiscardRow: (group: CanvasNodeDiffGroup, path: string) => void;
}) {
  return (
    <TableRow className="bg-transparent hover:bg-transparent">
      <TableCell>
        <div className="flex items-center gap-3">
          <Badge variant={getKindBadgeVariant(row.kind)}>
            {getKindIcon(row.kind)}
          </Badge>
          <span className={cn(getKindTextClassName(row.kind))}>{row.label}</span>
          {row.derivedFrom ? (
            <span className="text-muted-foreground">
              derived from {row.derivedFrom.resourceName}
            </span>
          ) : null}
        </div>
      </TableCell>
      {showCurrentValue ? (
        <TableCell>
          <ValueCell kind={row.kind} value={row.currentValue} tone="current" />
        </TableCell>
      ) : null}
      {showNewValue ? (
        <TableCell>
          <ValueCell kind={row.kind} value={row.newValue} tone="new" />
        </TableCell>
      ) : null}
      <TableCell>
        {row.canDiscard ? (
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => {
              if (totalChanges === 1) {
                onCloseDialog();
              }

              onDiscardRow(group, row.path);
            }}
          >
            <Trash2Icon />
            <span className="sr-only">Discard {row.label}</span>
          </Button>
        ) : null}
      </TableCell>
    </TableRow>
  );
}
