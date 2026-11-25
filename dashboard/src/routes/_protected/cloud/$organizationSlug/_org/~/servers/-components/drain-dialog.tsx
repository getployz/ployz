import { ConfirmDialog } from "#/components/confirm-dialog";
import { Badge } from "#/components/ui/badge";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { type DrainDialogRow, globalsStopDrain } from "#/modules/machines/server-drain-view";

const STAYS = {
  global: { badge: "Global", variant: "destructive", why: "Runs on every server" },
  data: { badge: "Stays", variant: "warning", why: "Data on this server" },
  unowned: { badge: "Skipped", variant: "secondary", why: "No project owns it" },
} as const satisfies Record<NonNullable<DrainDialogRow["stays"]>, { badge: string; variant: "destructive" | "warning" | "secondary"; why: string }>;

const WITH_DATA = { badge: "Moves with data", variant: "secondary", why: "Its volume moves with it" } as const;

/** "it mounts data and cache" as the dialog's line: "Stays: it mounts data and cache". */
const staysWhy = (why: string) => `Stays: ${why}`;

/** The plain confirm: what runs here, and which of it won't move. A Global refuses the Drain, so it can't be confirmed. No dry run. */
export function DrainDialog({ serverName, rows, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** What runs here, the ones a Drain won't move last (`drainDialogRows`). */
  rows: readonly DrainDialogRow[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const moves = rows.some((row) => row.stays === null);
  const withData = rows.some((row) => row.withData);
  const globals = rows.filter((row) => row.stays === "global").map((row) => row.name);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Drain ${serverName}`}
      description={
        <div className="flex flex-col gap-4">
          <p>Moves its services to your other servers, one at a time. Nothing moves back on its own.</p>
          {rows.length === 0 ? (
            <p>Nothing is running on {serverName}.</p>
          ) : (
            <>
              <ItemGroup aria-label={`Running on ${serverName}`} className="max-h-72 overflow-y-auto text-foreground">
                {rows.map((row) => {
                  const badge = row.withData ? WITH_DATA : row.stays === null ? null : STAYS[row.stays];
                  const why = row.why === null ? badge?.why : staysWhy(row.why);
                  return (
                    <Item key={row.key} variant="outline" size="xs" data-row={row.key}>
                      <ItemContent className="min-w-0">
                        <ItemTitle>{row.name}</ItemTitle>
                        <ItemDescription>{why ?? row.namespace}</ItemDescription>
                      </ItemContent>
                      {badge === null ? null : <ItemActions><Badge variant={badge.variant}>{badge.badge}</Badge></ItemActions>}
                    </Item>
                  );
                })}
              </ItemGroup>
              {moves ? null : <p>Nothing to move.</p>}
            </>
          )}
          {globals.length > 0 ? (
            <p className="text-destructive">{globalsStopDrain(globals)}</p>
          ) : (
            <p>
              {withData
                ? `A service with a volume here moves with its data, one at a time. It stops while its last changes copy.`
                : `A service whose data can't move with it stays on ${serverName}.`}
            </p>
          )}
        </div>
      }
      actionLabel="Drain"
      actionDisabled={globals.length > 0}
      onConfirm={onConfirm}
    />
  );
}
