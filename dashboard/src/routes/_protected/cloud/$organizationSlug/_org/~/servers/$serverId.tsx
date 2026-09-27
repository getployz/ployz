import { createFileRoute, Link } from "@tanstack/react-router";
import { BoxIcon, ChevronRightIcon, TriangleAlertIcon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { DashboardPage } from "#/components/dashboard-page";
import { ServerStatusLabel } from "#/components/server-status-label";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { needsAttention } from "#/modules/machines/server-status";
import { useServers, type Server } from "#/modules/machines/use-servers";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";
import { RemoveServerSection } from "./-components/remove-server-section";
import { runsHere } from "./-components/runs-here";
import { ServerBuildsSection } from "./-components/server-builds-section";
import { ServerSwitcher } from "./-components/server-switcher";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/$serverId",
)({
  staticData: { crumb: ServerSwitcher },
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, serverId } = Route.useParams();
  const { state, servers } = useServers(organizationSlug);
  const server = servers.find((candidate) => candidate.machine.id === serverId);

  if (state === "loading") {
    return <DashboardPage width="content"><ServersSkeleton /></DashboardPage>;
  }
  if (state === "unreachable" || (state === "stale" && !server)) {
    return <DashboardPage width="content"><ServersUnreachable /></DashboardPage>;
  }
  if (!server) {
    return (
      <DashboardPage width="content">
        <Empty variant="first-run">
          <EmptyHeader>
            <EmptyTitle>This server is no longer in your organization</EmptyTitle>
            <EmptyDescription>It may have been removed.</EmptyDescription>
          </EmptyHeader>
        </Empty>
      </DashboardPage>
    );
  }

  const stale = state === "stale";
  const { machine } = server;
  return (
    <DashboardPage width="content">
      <div className="flex min-w-0 flex-col gap-1">
        <h1 className="truncate text-xl font-semibold">{server.name}</h1>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm">
          <ServerStatusLabel status={server.status} stale={stale} />
          {machine.publicIp ? (
            <span className="inline-flex items-center gap-1 font-mono text-muted-foreground">
              {machine.publicIp}
              <CopyButton value={machine.publicIp} label="Copy address" size="icon-xs" variant="ghost" />
            </span>
          ) : null}
        </div>
      </div>
      {stale ? (
        <ServersStaleAlert />
      ) : server.status === "offline" ? (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertTitle>Can’t reach {server.name}</AlertTitle>
          <AlertDescription>Check that it’s powered on and online.</AlertDescription>
        </Alert>
      ) : null}
      <RunningHere organizationSlug={organizationSlug} server={server} servers={servers} stale={stale} />
      <ServerBuildsSection machine={machine} organizationSlug={organizationSlug} />
      <RemoveServerSection machine={machine} organizationSlug={organizationSlug} />
    </DashboardPage>
  );
}

/** The Services with a container on this Server. While it is offline, each says whether it still runs elsewhere. */
function RunningHere({ organizationSlug, server, servers, stale }: {
  organizationSlug: string;
  server: Server;
  servers: readonly Server[];
  stale: boolean;
}) {
  const { projects, environments } = useWorkspace(organizationSlug);
  const up = servers.filter((other) => other.machine.id !== server.machine.id && !needsAttention(other.status));

  return (
    <section aria-labelledby="running-here-heading">
      <ItemGroup>
        <ItemContent>
          <ItemTitle>
            <h2 id="running-here-heading">Running here</h2>
          </ItemTitle>
        </ItemContent>
        {server.services.length === 0 ? (
          <Item variant="outline">
            <ItemContent>
              <ItemDescription>{runsHere(server)}</ItemDescription>
            </ItemContent>
          </Item>
        ) : server.services.map((service) => {
          const { cloud } = service;
          const project = cloud ? projects.find((candidate) => candidate.slug === cloud.projectSlug) : undefined;
          const environment = cloud
            ? findEnvironment(projects, environments, { projectSlug: cloud.projectSlug, environmentSlug: cloud.environmentSlug })
            : undefined;
          const elsewhere = up.find((other) => service.machineIds.has(other.machine.id));
          return (
            <Item
              key={service.identity}
              variant="outline"
              size="sm"
              render={cloud ? (
                <Link
                  to="/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId"
                  params={{ organizationSlug, projectSlug: cloud.projectSlug, environmentSlug: cloud.environmentSlug, serviceId: cloud.id }}
                />
              ) : undefined}
            >
              <ItemMedia variant="icon">{cloud ? getServiceIcon(cloud) : <BoxIcon />}</ItemMedia>
              <ItemContent className="min-w-0">
                <ItemTitle>{service.name}</ItemTitle>
                {cloud ? (
                  <ItemDescription className="line-clamp-1">
                    {project?.name ?? cloud.projectSlug} · {environment?.name ?? cloud.environmentSlug}
                  </ItemDescription>
                ) : null}
              </ItemContent>
              <ItemActions>
                {server.status !== "offline" ? (
                  cloud ? <ChevronRightIcon className="size-4 text-muted-foreground" /> : null
                ) : elsewhere ? (
                  <ItemDescription>Still on {elsewhere.name}</ItemDescription>
                ) : (
                  <ServerStatusLabel status="offline" stale={stale}>Down</ServerStatusLabel>
                )}
              </ItemActions>
            </Item>
          );
        })}
      </ItemGroup>
    </section>
  );
}
