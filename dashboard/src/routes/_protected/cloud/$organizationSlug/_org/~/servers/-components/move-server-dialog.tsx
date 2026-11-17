import type { Region } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { regionName } from "#/modules/machines/server-regions";
import type { Leaving } from "#/modules/machines/server-move";

function destination(row: Leaving, regions: readonly Region[] | undefined) {
  const region = regionName(row.region, regions);
  const [only, ...more] = row.to;
  if (only === undefined) return `No server in ${region}`;
  return more.length === 0 ? `→ ${only}` : `→ ${row.to.length} servers in ${region}`;
}

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
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{to === null ? `Clear ${serverName}’s region` : `Move ${serverName} to ${to}`}</DialogTitle>
          <DialogDescription>These leave {serverName} on the next deploy.</DialogDescription>
        </DialogHeader>
        <ItemGroup role="list" aria-label={`Leaving ${serverName}`} className="max-h-72 overflow-y-auto">
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
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
          <Button onClick={onConfirm}>Move server</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
