import type { ReactNode } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import type { PresentedRow } from "#/modules/branches/branch-review";

/** One setting of one node: `before` is the receiver's value, `after` the one that would land. */
export function ChangeRowItem({ row, conflict, description, children }: {
  row: PresentedRow;
  /** Also changed on the receiving side since branching; names that side. */
  conflict?: string;
  /** Replaces the values line, e.g. a reason. */
  description?: ReactNode;
  /** #1159/#1160: a tick or value choice. */
  children?: ReactNode;
}) {
  return (
    <Item variant="outline" size="sm">
      <ItemContent className="min-w-0">
        <ItemTitle className="flex-wrap">
          {row.node}{row.label ? <span className="font-normal text-muted-foreground">· {row.label}</span> : null}
          {conflict ? <Badge variant="warning"><TriangleAlertIcon />Changed in {conflict} too</Badge> : null}
        </ItemTitle>
        {description ?? (row.after ? (
          <ItemDescription className="flex flex-wrap items-center gap-1.5 font-mono wrap-anywhere">
            {row.before ? <><span className="line-through">{row.before}</span><span aria-label="becomes">→</span></> : null}
            <span className="text-foreground">{row.after}</span>
          </ItemDescription>
        ) : null)}
      </ItemContent>
      {children ? <ItemActions>{children}</ItemActions> : null}
    </Item>
  );
}
