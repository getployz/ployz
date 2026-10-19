import { useState } from "react";
import { Link } from "@tanstack/react-router";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { RelativeTime } from "#/components/relative-time";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { TableCell, TableRow } from "#/components/ui/table";
import type { FleetTokenView } from "#/modules/machines/waiting/fleet-token";

export function TokenRow({ organizationSlug, token, onRevoke }: { organizationSlug: string; token: FleetTokenView; onRevoke: () => void }) {
  const [confirming, setConfirming] = useState(false);
  const stops = token.waiting > 0
    ? `${token.waiting} waiting ${token.waiting === 1 ? "server" : "servers"} won't join.`
    : "New servers can't join with it.";
  return (
    <TableRow>
      <TableCell>
        <div className="flex items-center gap-2">
          <span className="font-mono font-medium">{token.name}</span>
          {token.autoEnrollMax !== null ? <Badge variant="secondary">Auto up to {token.autoEnrollMax}</Badge> : null}
        </div>
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
        <span className="text-muted-foreground"> · {token.joined}{token.autoEnrollMax === null ? "" : `/${token.autoEnrollMax}`} joined</span>
      </TableCell>
      <TableCell className={token.expired ? "text-muted-foreground" : undefined}>
        {token.expired ? "Expired" : <RelativeTime date={new Date(token.expiresAt)} />}
      </TableCell>
      <TableCell className="text-muted-foreground">
        {token.lastUsedAt === null ? "Never" : <RelativeTime date={new Date(token.lastUsedAt)} />}
      </TableCell>
      <TableCell className="text-right">
        <Button size="sm" variant="ghost" className="text-destructive" onClick={() => setConfirming(true)}>Revoke</Button>
        <ConfirmDialog
          open={confirming}
          onOpenChange={setConfirming}
          title={`Revoke ${token.name}?`}
          description={`${stops} Joined ones stay.`}
          actionLabel="Revoke"
          variant="destructive"
          onConfirm={() => {
            setConfirming(false);
            onRevoke();
          }}
        />
      </TableCell>
    </TableRow>
  );
}
