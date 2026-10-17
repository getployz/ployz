import { Link } from "@tanstack/react-router";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { TableCell, TableRow } from "#/components/ui/table";
import type { FleetTokenView } from "#/modules/machines/waiting/fleet-token";

/** Every token's servers wait for review until automatic joins ship, so a configured join limit isn't shown. */
export function TokenRow({ organizationSlug, token, onRevoke }: { organizationSlug: string; token: FleetTokenView; onRevoke: () => void }) {
  return (
    <TableRow>
      <TableCell>
        <div className="font-mono font-medium">{token.name}</div>
        <div className="text-xs text-muted-foreground">
          <RelativeTime date={new Date(token.createdAt)} /> · {token.createdBy}
        </div>
      </TableCell>
      <TableCell className="tabular-nums">
        {token.waiting > 0 ? (
          <Link
            to="/cloud/$organizationSlug/~/servers"
            params={{ organizationSlug }}
            search={{ token: token.name }}
            className="underline-offset-2 hover:underline"
          >
            {token.waiting} waiting
          </Link>
        ) : <span className="text-muted-foreground">0 waiting</span>}
        <span className="text-muted-foreground"> · {token.joined} joined</span>
      </TableCell>
      <TableCell className={token.expired ? "text-muted-foreground" : undefined}>
        {token.expired ? "Expired" : <RelativeTime date={new Date(token.expiresAt)} />}
      </TableCell>
      <TableCell className="text-muted-foreground">
        {token.lastUsedAt === null ? "Never" : <RelativeTime date={new Date(token.lastUsedAt)} />}
      </TableCell>
      <TableCell className="text-right">
        <Button size="sm" variant="ghost" className="text-destructive" onClick={onRevoke}>Revoke</Button>
      </TableCell>
    </TableRow>
  );
}
