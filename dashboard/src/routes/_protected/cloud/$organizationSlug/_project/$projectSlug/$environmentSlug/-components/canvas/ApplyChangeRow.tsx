import { Trash2Icon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { TableCell, TableRow } from "#/components/ui/table";
import { cn } from "#/lib/utils";
import type { DiffRow } from "#/modules/services/service-deployment-diff/fields";
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
  onDiscard,
}: {
  row: Pick<DiffRow, "kind" | "label" | "currentValue" | "newValue">;
  tone: ApplyChangeTone;
  showCurrentValue: boolean;
  showNewValue: boolean;
  onDiscard?: () => void;
}) {
  return (
    // On phones an applied row stacks old above new, so long values such as image refs get the full width.
    <TableRow className={cn("bg-transparent hover:bg-transparent", tone === "applied" && "max-wf-nav:flex max-wf-nav:flex-col max-wf-nav:py-1")}>
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
        </TableCell>
      ) : null}
      {tone === "staged" ? (
        <TableCell>
          {onDiscard ? (
            <Button variant="ghost" size="icon-sm" onClick={onDiscard}>
              <Trash2Icon />
              <span className="sr-only">Discard {row.label}</span>
            </Button>
          ) : null}
        </TableCell>
      ) : null}
    </TableRow>
  );
}
