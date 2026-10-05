import { ConfirmDialog } from "#/components/confirm-dialog";
import { Badge } from "#/components/ui/badge";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import type { DrainDialogRow } from "#/modules/machines/server-drain-view";

const STAYS = {
  data: { badge: "Stays", variant: "warning", why: "Data on this server" },
  unowned: { badge: "Skipped", variant: "secondary", why: "No project owns it" },
} as const satisfies Record<NonNullable<DrainDialogRow["stays"]>, { badge: string; variant: "warning" | "secondary"; why: string }>;

/** The plain confirm: what runs here, and which of it won't move. No dry run. */
export function DrainDialog({ serverName, rows, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** What runs here, the ones a Drain won't move last (`drainDialogRows`). */
  rows: readonly DrainDialogRow[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const moves = rows.some((row) => row.stays === null);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Drain ${serverName}`}
      description={
        <div className="flex flex-col gap-4">
          <p>Moves every service to your other servers, one at a time. Nothing moves back on its own.</p>
          {rows.length === 0 ? (
            <p>Nothing is running on {serverName}.</p>
          ) : (
            <>
              <ItemGroup aria-label={`Running on ${serverName}`} className="max-h-72 overflow-y-auto text-foreground">
                {rows.map((row) => {
                  const stays = row.stays === null ? null : STAYS[row.stays];
                  return (
                    <Item key={row.key} variant="outline" size="xs" data-row={row.key}>
                      <ItemContent className="min-w-0">
                        <ItemTitle>{row.name}</ItemTitle>
                        <ItemDescription>{stays?.why ?? row.namespace}</ItemDescription>
                      </ItemContent>
                      {stays === null ? null : <ItemActions><Badge variant={stays.variant}>{stays.badge}</Badge></ItemActions>}
                    </Item>
                  );
                })}
              </ItemGroup>
              {moves ? null : <p>Nothing to move.</p>}
            </>
          )}
          <p>Services with a volume on {serverName} stay.</p>
        </div>
      }
      actionLabel="Drain"
      onConfirm={onConfirm}
    />
  );
}
