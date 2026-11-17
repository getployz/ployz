import type { Region } from "@ployz/sdk";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { regionName } from "#/modules/machines/server-regions";
import type { Leaving } from "#/modules/machines/server-move";

/** Where a row goes: the one Server in its region, how many there are, or that there's none. */
function destination(row: Leaving, regions: readonly Region[] | undefined) {
  const region = regionName(row.region, regions);
  const [only, ...more] = row.to;
  if (only === undefined) return `No server in ${region}`;
  return more.length === 0 ? `→ ${only}` : `→ ${row.to.length} servers in ${region}`;
}

/** Asks before a Server changes region when Services leave it for the region their Environment names. */
export function MoveServerDialog({ serverName, regionName: to, rows, regions, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** The region's name, or null when the Server leaves every region. */
  regionName: string | null;
  rows: readonly Leaving[];
  regions: readonly Region[] | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={to === null ? `Clear ${serverName}’s region` : `Move ${serverName} to ${to}`}
      description={
        <div className="flex flex-col gap-4">
          <p>These leave {serverName} on the next deploy.</p>
          <ItemGroup role="list" aria-label={`Leaving ${serverName}`} className="max-h-72 overflow-y-auto text-foreground">
            {rows.map((row) => (
              <Item key={row.key} role="listitem" variant="outline" size="xs">
                <ItemContent className="min-w-0">
                  <ItemTitle>{row.volume === null ? row.name : `${row.name} · ${row.volume}`}</ItemTitle>
                  <ItemDescription className="font-mono">{row.environment}</ItemDescription>
                </ItemContent>
                <ItemActions><ItemDescription>{destination(row, regions)}</ItemDescription></ItemActions>
              </Item>
            ))}
          </ItemGroup>
        </div>
      }
      actionLabel="Move server"
      onConfirm={onConfirm}
    />
  );
}
