import { useState, type ReactNode } from "react";
import { ChevronDownIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "#/components/ui/avatar";
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
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { ApplyChangeRow } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/ApplyChangeRow";
import {
  getCanvasNodeIcon,
  getKindTextClassName,
  getServiceChangeAction,
  getSettingsLabel,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/apply-changes-display";

export function ApplyChangeGroupCard({
  group,
  totalChanges,
  visibleGroupCount,
  onCloseDialog,
  onDiscardNode,
  onDiscardRow,
  noteFor,
}: {
  group: ChangeGroup;
  totalChanges: number;
  visibleGroupCount: number;
  onCloseDialog: () => void;
  onDiscardNode: (group: ChangeGroup) => void;
  onDiscardRow: (group: ChangeGroup, path: string) => void;
  noteFor?: (path: string) => ReactNode;
}) {
  const [isExpanded, setIsExpanded] = useState(true);
  const nodeKind = group.lifecycle === "create" ? "add" : group.lifecycle === "delete" ? "remove" : "update";
  const nodeAction = getServiceChangeAction(nodeKind);
  const showCurrentValue = nodeKind !== "add";
  const showNewValue = nodeKind !== "remove";
  const hasSettings = group.rows.length > 0;
  const canDiscard =
    group.canDiscard &&
    (hasSettings ||
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
      <Card size="sm">
        <CardHeader>
          <div className="grid min-h-14 grid-cols-[minmax(0,1fr)_auto] items-center gap-2">
            {hasSettings ? (
              <CollapsibleTrigger
                render={
                  <Button
                    className="h-auto min-w-0 justify-start"
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
              {hasSettings ? (
                <div className="flex flex-col items-end text-sm">
                  <span className="text-muted-foreground">
                    {getSettingsLabel(group.rows.length)}
                  </span>
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
              <CardContent>
                <Table>
                  {/* On phones each row stacks its own values: no columns to head. */}
                  <TableHeader className="max-wf-nav:hidden">
                    <TableRow>
                      <TableHead>
                        Change
                      </TableHead>
                      {showCurrentValue ? (
                        <TableHead>
                          Current Value
                        </TableHead>
                      ) : null}
                      {showNewValue ? (
                        <TableHead>
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
                        row={row}
                        tone="staged"
                        showCurrentValue={showCurrentValue}
                        showNewValue={showNewValue}
                        note={noteFor?.(row.path)}
                        onDiscard={row.canDiscard ? () => {
                          if (totalChanges === 1) {
                            onCloseDialog();
                          }

                          onDiscardRow(group, row.path);
                        } : undefined}
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
