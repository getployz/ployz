import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { toast } from "sonner";
import { DashboardPage } from "#/components/dashboard-page";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { toErrorMessage } from "#/lib/error-message";
import type { FleetTokenView } from "#/modules/machines/waiting/fleet-token";
import { revokeFleetTokenServerFn } from "#/modules/machines/waiting/fleet-token.functions";
import { fleetTokensQueryOptions, waitingKeys } from "#/modules/machines/waiting/waiting.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServersDialog } from "./-components/add-servers-dialog";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/tokens",
)({
  loader: ({ params, context }) => prefetchRemote(context, fleetTokensQueryOptions(params.organizationSlug)),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { data: tokens = [] } = useQuery(fleetTokensQueryOptions(organizationSlug));
  const revoke = useRevokeToken(organizationSlug);

  return (
    <DashboardPage>
      <div className="flex items-center gap-3">
        <div className="flex flex-col gap-0.5">
          <h1 className="text-lg font-semibold">Fleet tokens</h1>
          <p className="text-sm text-muted-foreground">Paste one into any server and it shows up here.</p>
        </div>
        <div className="ml-auto">
          <AddServersDialog organizationSlug={organizationSlug} label="New token" />
        </div>
      </div>
      <div className="overflow-x-auto rounded-lg border">
        <Table aria-label="Fleet tokens">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Joins</TableHead>
              <TableHead>Servers</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {tokens.length === 0 ? (
              <TableRow>
                <TableCell colSpan={6} className="text-muted-foreground">No tokens yet</TableCell>
              </TableRow>
            ) : tokens.map((token) => (
              <TokenRow key={token.id} organizationSlug={organizationSlug} token={token} onRevoke={() => revoke.mutate(token)} />
            ))}
          </TableBody>
        </Table>
      </div>
      <p className="text-xs text-muted-foreground">Expired or revoked tokens stop new servers. Ones already in stay.</p>
    </DashboardPage>
  );
}

function TokenRow({ organizationSlug, token, onRevoke }: { organizationSlug: string; token: FleetTokenView; onRevoke: () => void }) {
  return (
    <TableRow>
      <TableCell>
        <div className="font-mono font-medium">{token.name}</div>
        <div className="text-xs text-muted-foreground">
          <RelativeTime date={new Date(token.createdAt)} /> · {token.createdBy}
        </div>
      </TableCell>
      <TableCell>{token.autoEnrollMax === null ? "When you say" : `Auto up to ${token.autoEnrollMax}`}</TableCell>
      <TableCell className="tabular-nums">
        {token.autoEnrollMax === null ? (
          <>
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
          </>
        ) : `${token.joined}/${token.autoEnrollMax} joined`}
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

function useRevokeToken(organizationSlug: string) {
  const queryClient = useQueryClient();
  const { queryKey } = fleetTokensQueryOptions(organizationSlug);
  return useMutation({
    mutationFn: (token: FleetTokenView) => revokeFleetTokenServerFn({ data: { organizationSlug, id: token.id } }),
    onMutate: async (token) => {
      await queryClient.cancelQueries({ queryKey });
      const previous = queryClient.getQueryData(queryKey);
      if (previous) queryClient.setQueryData(queryKey, previous.filter((row) => row.id !== token.id));
      return { previous };
    },
    onError: (cause, _token, context) => {
      if (context?.previous) queryClient.setQueryData(queryKey, context.previous);
      toast.error(toErrorMessage(cause, "Couldn't revoke."));
    },
    onSuccess: ({ waitingRemoved }, token) =>
      toast.success(`Revoked ${token.name}${waitingRemoved > 0 ? ` · ${waitingRemoved} waiting removed` : ""}`),
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey }),
      queryClient.invalidateQueries({ queryKey: waitingKeys.all }),
    ]),
  });
}
