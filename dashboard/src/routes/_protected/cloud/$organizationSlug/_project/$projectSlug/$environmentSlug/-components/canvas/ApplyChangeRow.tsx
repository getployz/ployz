import type { ReactNode } from "react";
import { PinIcon, Trash2Icon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { TableCell, TableRow } from "#/components/ui/table";
import type { ChangeRow } from "#/modules/config-store/store-deployments";
import {
  getKindBadgeVariant,
  getKindIcon,
  getKindTextClassName,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/apply-changes-display";
import { ApplyChangeValueCell as ValueCell, type ApplyChangeTone } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeValueCell";

/** One setting change. Staged rows carry Intent Pink and may be discarded; applied rows (what an attempt deployed) stay neutral. */
export function ApplyChangeRow({
  row,
  tone,
  showCurrentValue,
  showNewValue,
  note,
  onDiscard,
  onNeverSync,
}: {
  row: Pick<ChangeRow, "kind" | "label" | "currentValue" | "newValue">;
  tone: ApplyChangeTone;
  showCurrentValue: boolean;
  showNewValue: boolean;
  /** Under the new value, such as the Environment or pull request it came from. */
  note?: ReactNode;
  onDiscard?: () => void;
  /** It arrived from another Environment: mark it Never sync, and it goes. */
  onNeverSync?: () => void;
}) {
  return (
    // On phones a row stacks old above new, so long values such as image refs get the full width.
    <TableRow className="bg-transparent hover:bg-transparent max-wf-nav:flex max-wf-nav:flex-col max-wf-nav:py-1 max-wf-nav:[&>td]:w-full">
      <TableCell>
        <div className="flex items-center gap-3">
          <Badge variant={tone === "staged" ? getKindBadgeVariant(row.kind) : "outline"}>
            {getKindIcon(row.kind)}
          </Badge>
          <span className={tone === "staged" ? getKindTextClassName(row.kind) : undefined}>{row.label}</span>
        </div>
      </TableCell>
      {showCurrentValue ? (
        <TableCell>
          <ValueCell kind={row.kind} value={row.currentValue} tone={tone} side="current" />
        </TableCell>
      ) : null}
      {showNewValue ? (
        <TableCell>
          <ValueCell kind={row.kind} value={row.newValue} tone={tone} side="new" />
          {note ? <div className="mt-1">{note}</div> : null}
        </TableCell>
      ) : null}
      {tone === "staged" ? (
        <TableCell>
          <div className="flex items-center justify-end gap-1">
            {onNeverSync ? (
              <Button variant="ghost" size="sm" aria-label={`Never sync ${row.label}`} onClick={onNeverSync}>
                <PinIcon data-icon="inline-start" />Never sync
              </Button>
            ) : null}
            {onDiscard ? (
              <Button variant="ghost" size="icon-sm" onClick={onDiscard}>
                <Trash2Icon />
                <span className="sr-only">Discard {row.label}</span>
              </Button>
            ) : null}
          </div>
        </TableCell>
      ) : null}
    </TableRow>
  );
}
