import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { useRevokeFleetToken } from "#/modules/machines/waiting/use-waiting";
import { fleetTokensQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServersDialog } from "./-components/add-servers-dialog";
import { TokenRow } from "./-components/token-row";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/tokens",
)({
  staticData: { section: "servers" },
  loader: ({ params, context }) => prefetchRemote(context, fleetTokensQueryOptions(params.organizationSlug)),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { data: tokens = [] } = useQuery(fleetTokensQueryOptions(organizationSlug));
  const revoke = useRevokeFleetToken(organizationSlug);

  return (
    <DashboardPage title="Fleet tokens" actions={<AddServersDialog organizationSlug={organizationSlug} label="New token" />}>
      <p className="text-sm text-muted-foreground">Servers wait for review, or join an existing cluster on arrival with an Auto token.</p>
      <div className="overflow-x-auto rounded-lg border">
        <Table aria-label="Fleet tokens">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Servers</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead>Last used</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {tokens.length === 0 ? (
              <TableRow>
                <TableCell colSpan={5} className="text-muted-foreground">No tokens yet</TableCell>
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
