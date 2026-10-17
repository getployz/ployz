import { createFileRoute, Link } from "@tanstack/react-router";
import { Schema } from "effect";
import { ServerIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { useServers } from "#/modules/machines/use-servers";
import { serverEnrollmentsQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { NEWEST_RELEASE } from "#/modules/server-upgrade/server-upgrade";
import { channelReleaseQueryOptions, latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServersDialog } from "./-components/add-servers-dialog";
import { SERVER_LIST, ServerRow } from "./-components/server-row";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";
import { ServersUpgradeBar, useServersUpgrade } from "./-components/servers-upgrade";
import { WaitingSection } from "./-components/waiting-list";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ token: Schema.optional(Schema.String) })),
  // The release on the Servers' own line waits for the versions they report.
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    channelReleaseQueryOptions(NEWEST_RELEASE),
    waitingServersQueryOptions(params.organizationSlug),
    serverEnrollmentsQueryOptions(params.organizationSlug),
  ),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { token } = Route.useSearch();
  const { state, servers } = useServers(organizationSlug);
  const upgrades = useServersUpgrade(organizationSlug, servers);
  const upgrade = state === "live" && servers.length > 0 ? upgrades : null;

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; each row carries its own status, so this bar holds only actions. */}
      <div className="flex items-center justify-end gap-2">
        {upgrade === null ? null : <ServersUpgradeBar organizationSlug={organizationSlug} upgrade={upgrade} />}
        <Button variant="ghost" nativeButton={false} render={<Link to="/cloud/$organizationSlug/~/servers/tokens" params={{ organizationSlug }} />}>
          Tokens
        </Button>
        <AddServersDialog
          organizationSlug={organizationSlug}
          label={upgrade === null ? "Add servers" : <span className="max-sm:sr-only">Add servers</span>}
        />
      </div>
      {/* Above the list: the skeleton below is a guess at the list's height, and Waiting must not jump when it settles. */}
      <WaitingSection organizationSlug={organizationSlug} token={token} hasFirst={servers.length > 0} />
      {state === "loading" ? (
        <ServersSkeleton />
      ) : state === "unreachable" ? (
        <ServersUnreachable organizationSlug={organizationSlug} />
      ) : servers.length === 0 ? (
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyMedia variant="icon"><ServerIcon /></EmptyMedia>
            <EmptyTitle>Add your first server</EmptyTitle>
            <EmptyDescription>Run one line on any Linux servers. They wait here for you to review.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      ) : (
        <>
          {state === "stale" ? <ServersStaleAlert /> : null}
          <nav aria-label="Servers" className={SERVER_LIST}>
            {servers.map((server) => (
              <ServerRow
                key={server.machine.id}
                organizationSlug={organizationSlug}
                server={server}
                upgrade={upgrade?.rows.get(server.machine.id) ?? null}
                stale={state === "stale"}
              />
            ))}
          </nav>
        </>
      )}
    </DashboardPage>
  );
}
