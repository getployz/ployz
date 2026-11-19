import type { NamedRegion } from "@ployz/sdk";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { regionName } from "#/modules/machines/server-regions";
import type { Leaving } from "#/modules/machines/server-move";

const named = (keys: readonly string[], regions: readonly NamedRegion[] | undefined) =>
  keys.map((each) => regionName(each, regions)).join(", ");

function next(row: Leaving, regions: readonly NamedRegion[] | undefined) {
  if (row.next === "waits") return `Waits for a server in ${named(row.empty, regions)}`;
  const [only, ...more] = row.to;
  return more.length === 0 ? `→ ${only}` : `→ ${row.to.length} servers in ${named(row.searched, regions)}`;
}

function summary(serverName: string, rows: readonly Leaving[]) {
  const waiting = rows.filter((row) => row.next === "waits").length;
  if (rows.length === 0) return `Nothing shown leaves ${serverName}.`;
  if (waiting === 0) return `These leave ${serverName} on the next deploy.`;
  return waiting === rows.length
    ? `These keep running on ${serverName} until their region has a server.`
    : `These leave ${serverName} on the next deploy. The ones that wait keep running here.`;
}

export function MoveServerDialog({ serverName, regionName: to, rows, unread, holdReason, regions, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** The region's name, or null when the Server leaves every region. */
  regionName: string | null;
  rows: readonly Leaving[];
  /** The Environments whose Working State couldn't be read: what leaves from them isn't shown. */
  unread: readonly string[];
  holdReason: string | null;
  regions: readonly NamedRegion[] | undefined;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{to === null ? `Clear ${serverName}’s region` : `Move ${serverName} to ${to}`}</DialogTitle>
          <DialogDescription>
            {holdReason ?? summary(serverName, rows)}
            {holdReason === null && unread.length > 0 ? ` Couldn’t read ${unread.join(", ")}, so what leaves from there isn’t shown.` : ""}
          </DialogDescription>
        </DialogHeader>
        <ItemGroup role="list" aria-label="Services this changes" className="max-h-72 overflow-y-auto">
          {rows.map((row) => (
            <Item key={row.key} role="listitem" variant="outline" size="xs">
              <ItemContent className="min-w-0">
                <ItemTitle>{row.name}</ItemTitle>
                <ItemDescription className="font-mono">{row.environment}</ItemDescription>
              </ItemContent>
              <ItemActions><ItemDescription>{next(row, regions)}</ItemDescription></ItemActions>
            </Item>
          ))}
        </ItemGroup>
        <DialogFooter>
          <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
          <Button disabled={holdReason !== null} onClick={onConfirm}>{to === null ? "Clear region" : "Move server"}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
