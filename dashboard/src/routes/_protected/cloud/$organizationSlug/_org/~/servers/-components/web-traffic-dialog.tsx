import { ConfirmDialog } from "#/components/confirm-dialog";
import { Item, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";
import type { WebTrafficReach } from "#/modules/machines/server-web-traffic-view";

const empty = {
  away: "This stops the web server still running here.",
  last: "No other server takes web traffic.",
  shared: "Your other servers keep taking web traffic.",
} satisfies Record<WebTrafficReach, string>;

const listed = {
  away: "This stops the web server still running here. These may still point at it:",
  last: "No other server takes web traffic. These stop working:",
  shared: "Your other servers keep serving these:",
} satisfies Record<WebTrafficReach, string>;

/** The confirm before web traffic goes off: which domains this affects, and whether another Server keeps serving them. */
export function WebTrafficDialog({ serverName, domains, partial, reach, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** Every domain pointed at your Servers. */
  domains: readonly string[];
  /** Some domains weren't observed, so the list may be short. */
  partial: boolean;
  reach: WebTrafficReach;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Stop web traffic on ${serverName}?`}
      description={
        <div className="flex flex-col gap-4">
          {domains.length === 0 ? (
            <p>{empty[reach]}</p>
          ) : (
            <>
              <p>{listed[reach]}</p>
              <ItemGroup aria-label="Domains" className="max-h-72 overflow-y-auto text-foreground">
                {domains.map((domain) => (
                  <Item key={domain} variant="outline" size="xs" data-row={domain}>
                    <ItemContent className="min-w-0">
                      <ItemTitle className="font-mono">{domain}</ItemTitle>
                    </ItemContent>
                  </Item>
                ))}
              </ItemGroup>
            </>
          )}
          {partial ? <p>Some servers haven’t reported their domains yet, so this may affect more.</p> : null}
        </div>
      }
      actionLabel="Stop"
      variant={reach === "last" ? "destructive" : "default"}
      onConfirm={onConfirm}
    />
  );
}
