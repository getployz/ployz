import { ConfirmDialog } from "#/components/confirm-dialog";
import { Item, ItemContent, ItemGroup, ItemTitle } from "#/components/ui/item";

/** The confirm before web traffic goes off: which domains this affects, and whether another Server keeps serving them. */
export function WebTrafficDialog({ serverName, domains, last, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** Every domain pointed at your Servers. */
  domains: readonly string[];
  /** No other Server takes web traffic: these stop working. */
  last: boolean;
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
            <p>{last ? "No other server takes web traffic." : "Your other servers keep taking web traffic."}</p>
          ) : (
            <>
              <p>{last ? "No other server takes web traffic. These stop working:" : "Your other servers keep serving these:"}</p>
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
        </div>
      }
      actionLabel="Stop"
      variant={last ? "destructive" : "default"}
      onConfirm={onConfirm}
    />
  );
}
