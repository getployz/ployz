import { createFileRoute } from "@tanstack/react-router";
import { ServerIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { useServers } from "#/modules/machines/use-servers";
import { NEWEST_RELEASE } from "#/modules/server-upgrade/server-upgrade";
import { channelReleaseQueryOptions, latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServerDialog } from "./-components/add-server-dialog";
import { SERVER_LIST, ServerRow } from "./-components/server-row";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";
import { ServersUpgradeBar, useServersUpgrade } from "./-components/servers-upgrade";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  // The release on the Servers' own line waits for the versions they report.
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    channelReleaseQueryOptions(NEWEST_RELEASE),
  ),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { state, servers } = useServers(organizationSlug);
  const upgrades = useServersUpgrade(organizationSlug, servers);
  const upgrade = state === "live" && servers.length > 0 ? upgrades : null;

  return (
    <DashboardPage width="content">
      {/* The top bar names the page; each row carries its own status, so this bar holds only actions. */}
      <div className="flex items-center justify-end gap-2">
        {upgrade === null ? <AddServerDialog organizationSlug={organizationSlug} /> : (
          <>
            <ServersUpgradeBar organizationSlug={organizationSlug} upgrade={upgrade} />
            {/* Beside the upgrade controls a phone has room for the icon only; the label stays the button's name. */}
            <AddServerDialog organizationSlug={organizationSlug} label={<span className="max-sm:sr-only">Add server</span>} />
          </>
        )}
      </div>
      {state === "loading" ? (
        <ServersSkeleton />
      ) : state === "unreachable" ? (
        <ServersUnreachable organizationSlug={organizationSlug} />
      ) : servers.length === 0 ? (
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyMedia variant="icon"><ServerIcon /></EmptyMedia>
            <EmptyTitle>Add your first server</EmptyTitle>
            <EmptyDescription>Run one command on a Linux server and it joins this organization.</EmptyDescription>
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
