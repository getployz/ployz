import { useState } from "react";
import { ChevronDownIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card, CardContent, CardHeader } from "#/components/ui/card";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "#/components/ui/collapsible";
import { Separator } from "#/components/ui/separator";
import {
  Table,
  TableBody,
  TableHead,
  TableHeader,
  TableRow,
} from "#/components/ui/table";
import { cn } from "#/lib/utils";
import {
  getCanvasNodeChangeKind,
  getCanvasNodeDiffGroupCanDiscard,
} from "#/modules/environment-design/canvas-node-diff";
import type { CanvasNodeDiffGroup } from "#/modules/environment-design/canvas-node-diff";
import { countOwnedRows } from "#/modules/services/service-deployment-diff/fields";
import { ApplyChangeRow } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeRow";
import {
  getCanvasNodeIcon,
  getKindTextClassName,
  getServiceChangeAction,
  getSettingsLabel,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/apply-changes-display";

type DisplayCanvasNodeDiffGroup = CanvasNodeDiffGroup & {
  slice?: "unsaved" | "pending" | "drift";
};

function getDerivedRowCount(group: DisplayCanvasNodeDiffGroup) {
  return group.rows.filter((row) => row.derivedFrom).length;
}

export function ApplyChangeGroupCard({
  group,
  totalChanges,
  visibleGroupCount,
  onCloseDialog,
  onDiscardNode,
  onDiscardRow,
}: {
  group: DisplayCanvasNodeDiffGroup;
  totalChanges: number;
  visibleGroupCount: number;
  onCloseDialog: () => void;
  onDiscardNode: (group: CanvasNodeDiffGroup) => void;
  onDiscardRow: (group: CanvasNodeDiffGroup, path: string) => void;
}) {
  const [isExpanded, setIsExpanded] = useState(true);
  const nodeKind = getCanvasNodeChangeKind(group);
  const nodeAction = getServiceChangeAction(nodeKind);
  const showCurrentValue = nodeKind !== "add";
  const showNewValue = nodeKind !== "remove";
  const ownedRowCount = countOwnedRows(group.rows);
  const derivedRowCount = getDerivedRowCount(group);
  const hasSettings = group.rows.length > 0;
  const canDiscard =
    getCanvasNodeDiffGroupCanDiscard(group) &&
    (ownedRowCount > 0 ||
      group.lifecycle === "create" ||
      group.lifecycle === "delete");
  const nodeIdentity = (
    <>
      <Avatar>
        <AvatarFallback>{getCanvasNodeIcon(group)}</AvatarFallback>
      </Avatar>
      <div className="flex min-w-0 items-center gap-2 overflow-hidden">
        <span
          className={cn(
            "truncate font-medium",
            getKindTextClassName(nodeKind),
          )}
        >
          {group.nodeName}
        </span>
        <span
          className={cn(
            "hidden truncate sm:inline",
            getKindTextClassName(nodeKind),
          )}
        >
          {nodeAction}
        </span>
      </div>
    </>
  );

  return (
    <Collapsible open={isExpanded} onOpenChange={setIsExpanded}>
      <Card className="gap-0 bg-background py-0" size="sm">
        <CardHeader className="px-0">
          <div className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-2 p-3">
            {hasSettings ? (
              <CollapsibleTrigger
                render={
                  <Button
                    className="h-auto min-w-0 justify-start gap-3 bg-transparent px-0 hover:bg-transparent aria-expanded:bg-transparent aria-expanded:text-foreground dark:hover:bg-transparent"
                    variant="ghost"
                  />
                }
              >
                <ChevronDownIcon
                  className={cn(
                    "transition-transform",
                    isExpanded ? "rotate-0" : "-rotate-90",
                  )}
                />
                <span className="sr-only">Toggle details</span>
                {nodeIdentity}
              </CollapsibleTrigger>
            ) : (
              <div className="flex min-w-0 items-center gap-3">
                {nodeIdentity}
              </div>
            )}

            <div className="flex items-center gap-4">
              {group.slice ? (
                <Badge variant={group.slice === "unsaved" ? "changed" : "secondary"}>
                  {group.slice === "unsaved"
                    ? "Unsaved"
                    : group.slice === "pending"
                      ? "Pending"
                      : "Drift"}
                </Badge>
              ) : null}
              {hasSettings ? (
                <div className="flex flex-col items-end text-sm">
                  {ownedRowCount > 0 ? (
                    <span className="text-muted-foreground">
                      {getSettingsLabel(ownedRowCount)}
                    </span>
                  ) : null}
                  {derivedRowCount > 0 ? (
                    <span className="text-muted-foreground">
                      {derivedRowCount} derived
                    </span>
                  ) : null}
                </div>
              ) : null}
              <Button
                variant="outline"
                disabled={!canDiscard}
                onClick={() => {
                  if (visibleGroupCount === 1) {
                    onCloseDialog();
                  }

                  onDiscardNode(group);
                }}
              >
                Discard
              </Button>
            </div>
          </div>
        </CardHeader>

        {hasSettings ? (
          <CollapsibleContent>
            <div className="bg-muted/40">
              <Separator />
              <CardContent className="px-0">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead className="text-muted-foreground">
                        Change
                      </TableHead>
                      {showCurrentValue ? (
                        <TableHead className="text-muted-foreground">
                          Current Value
                        </TableHead>
                      ) : null}
                      {showNewValue ? (
                        <TableHead className="text-muted-foreground">
                          New Value
                        </TableHead>
                      ) : null}
                      <TableHead className="w-14" />
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {group.rows.map((row) => (
                      <ApplyChangeRow
                        key={row.changeKey}
                        group={group}
                        row={row}
                        totalChanges={totalChanges}
                        showCurrentValue={showCurrentValue}
                        showNewValue={showNewValue}
                        onCloseDialog={onCloseDialog}
                        onDiscardRow={onDiscardRow}
                      />
                    ))}
                  </TableBody>
                </Table>
              </CardContent>
            </div>
          </CollapsibleContent>
        ) : null}
      </Card>
    </Collapsible>
  );
}
