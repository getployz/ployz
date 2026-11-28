import { createFileRoute } from "@tanstack/react-router";
import { DashboardPage } from "#/components/dashboard-page";
import { OPERATE_REFUSED } from "#/modules/machines/server-policy.hooks";
import { regionLabels } from "#/modules/machines/server-regions";
import { useRegions } from "#/modules/machines/server-regions.hooks";
import { useServers } from "#/modules/machines/use-servers";
import { NEWEST_RELEASE } from "#/modules/server-upgrade/server-upgrade";
import { channelReleaseQueryOptions, latestServerUpgradesQueryOptions } from "#/modules/server-upgrade/server-upgrade.queries";
import { prefetchRemote } from "#/collections/route-data";
import { AddServerDialog } from "#/modules/machines/add-server/add-server-dialog";
import { FirstRun } from "./-components/first-run-card";
import { FirstTunnelStatus } from "./-components/first-tunnel";
import { FleetHealth } from "./-components/fleet-health";
import { PendingJoins } from "./-components/pending-joins";
import { SERVER_LIST, ServerRow } from "./-components/server-row";
import { serversPageState } from "./-components/servers-page-state";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";
import { ServersUpgradeBar, useServersUpgrade } from "./-components/servers-upgrade";
import { organizationMembersQueryOptions, useRoleGate } from "#/modules/organization/members.queries";
import { RenameRegionDialog } from "./-components/rename-region-dialog";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  staticData: { section: "servers" },
  // The release on the Servers' own line waits for the versions they report.
  loader: ({ params, context }) => prefetchRemote(
    context,
    latestServerUpgradesQueryOptions(params.organizationSlug),
    channelReleaseQueryOptions(NEWEST_RELEASE),
    organizationMembersQueryOptions(params.organizationSlug),
  ),
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { state, servers, unreachableReason } = useServers(organizationSlug);
  const upgrades = useServersUpgrade(organizationSlug, servers);
  const labels = regionLabels(servers.map(({ machine }) => machine));
  const regions = useRegions(organizationSlug, labels);
  const operate = useRoleGate(organizationSlug, "servers:operate", OPERATE_REFUSED);
  const page = serversPageState({ list: state, servers: servers.length });
  const upgrade = page.kind === "fleet" && !page.stale ? upgrades : null;

  return (
    <DashboardPage width="content" title="Servers" actions={<>
      {upgrade === null ? null : <ServersUpgradeBar organizationSlug={organizationSlug} upgrade={upgrade} />}
      {page.kind === "fleet" && regions !== undefined && regions.length > 0
        ? <RenameRegionDialog organizationSlug={organizationSlug} regions={regions} labels={labels} gate={operate} />
        : null}
      {page.kind === "first-run" ? null : (
        <AddServerDialog
          organizationSlug={organizationSlug}
          label={upgrade === null ? "Add server" : <span className="max-sm:sr-only">Add server</span>}
        />
      )}
    </>}>
      {page.kind === "fleet" ? <FirstTunnelStatus organizationSlug={organizationSlug} /> : null}
      {page.kind === "fleet" ? <PendingJoins organizationSlug={organizationSlug} /> : null}
      {page.kind === "loading" ? (
        <ServersSkeleton />
      ) : page.kind === "unreachable" ? (
        <ServersUnreachable organizationSlug={organizationSlug} reason={unreachableReason} />
      ) : page.kind === "first-run" ? (
        <FirstRun key={organizationSlug} organizationSlug={organizationSlug} />
      ) : (
        <>
          {page.stale ? <ServersStaleAlert /> : null}
          <FleetHealth servers={servers} upgrades={upgrade?.rows ?? new Map()} />
          <nav aria-label="Servers" className={SERVER_LIST}>
            {servers.map((server) => (
              <ServerRow
                key={server.machine.id}
                organizationSlug={organizationSlug}
                server={server}
                regions={regions}
                upgrade={upgrade?.rows.get(server.machine.id) ?? null}
                stale={page.stale}
              />
            ))}
          </nav>
        </>
      )}
    </DashboardPage>
  );
}
