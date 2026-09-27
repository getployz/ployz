import { createFileRoute, Link } from "@tanstack/react-router";
import { BoxIcon, ChevronRightIcon, TriangleAlertIcon, WifiOffIcon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { DashboardPage } from "#/components/dashboard-page";
import { ServerStatusLabel } from "#/components/server-status-label";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Empty, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { findEnvironment, useWorkspace } from "#/modules/environment-design/workspace.queries";
import { useServers, type Server } from "#/modules/machines/use-servers";
import { getServiceIcon } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/service-node-helpers";
import { RemoveServerSection } from "./-components/remove-server-section";
import { ServerBuildsSection } from "./-components/server-builds-section";
import { ServersSkeleton } from "./-components/servers-skeleton";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/$serverId",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug, serverId } = Route.useParams();
  const { lens, stale, servers } = useServers(organizationSlug);
  const server = servers.find((candidate) => candidate.machine.id === serverId);

  if (lens === "connecting") {
    return <DashboardPage width="content"><ServersSkeleton /></DashboardPage>;
  }
  if (!server) {
    return (
      <DashboardPage width="content">
        <Empty variant="first-run">
          <EmptyHeader>
            {lens === "unreachable" ? (
              <>
                <EmptyTitle>Can’t reach your servers right now</EmptyTitle>
                <EmptyDescription>Ployz keeps trying and shows this server as soon as it can.</EmptyDescription>
              </>
            ) : (
              <>
                <EmptyTitle>This server is no longer in your organization</EmptyTitle>
                <EmptyDescription>It may have been removed.</EmptyDescription>
              </>
            )}
          </EmptyHeader>
        </Empty>
      </DashboardPage>
    );
  }

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
      <ServerProblem server={server} stale={stale} />
      <RunningHere organizationSlug={organizationSlug} server={server} servers={servers} />
      <ServerBuildsSection machine={machine} organizationSlug={organizationSlug} />
      <RemoveServerSection machine={machine} organizationSlug={organizationSlug} />
    </DashboardPage>
  );
}

/** One sentence when something is wrong; nothing when all is well. */
function ServerProblem({ server, stale }: { server: Server; stale: boolean }) {
  if (stale) {
    return (
      <Alert>
        <WifiOffIcon />
        <AlertTitle>Can’t reach your servers right now</AlertTitle>
        <AlertDescription>Showing what {server.name} last reported.</AlertDescription>
      </Alert>
    );
  }
  if (server.status === "offline") {
    return (
      <Alert variant="destructive">
        <TriangleAlertIcon />
        <AlertTitle>Can’t reach {server.name}</AlertTitle>
        <AlertDescription>Check that it’s powered on and online. If it’s gone for good, remove it below.</AlertDescription>
      </Alert>
    );
  }
  if (server.status === "not_responding") {
    return (
      <Alert>
        <TriangleAlertIcon />
        <AlertTitle>{server.name} is slow to answer</AlertTitle>
        <AlertDescription>This usually clears up on its own.</AlertDescription>
      </Alert>
    );
  }
  return null;
}

/** The Services with a container on this Server. While it is offline, each says whether it still runs elsewhere. */
function RunningHere({ organizationSlug, server, servers }: { organizationSlug: string; server: Server; servers: readonly Server[] }) {
  const { projects, environments } = useWorkspace(organizationSlug);
  const answering = servers.filter((other) => other.machine.id !== server.machine.id && other.status !== "offline");

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
              <ItemDescription>{server.machine.acceptsBuilds ? "No services yet. It runs builds." : "No services yet."}</ItemDescription>
            </ItemContent>
          </Item>
        ) : server.services.map((service) => {
          const { cloud } = service;
          const project = cloud ? projects.find((candidate) => candidate.slug === cloud.projectSlug) : undefined;
          const environment = cloud
            ? findEnvironment(projects, environments, { projectSlug: cloud.projectSlug, environmentSlug: cloud.environmentSlug })
            : undefined;
          const elsewhere = answering.find((other) => service.machineIds.has(other.machine.id));
          const content = (
            <>
              <ItemMedia variant="icon">{cloud ? getServiceIcon(cloud) : <BoxIcon />}</ItemMedia>
              <ItemContent className="min-w-0">
                <ItemTitle>{service.name}</ItemTitle>
                <ItemDescription className="line-clamp-1">
                  {cloud ? `${project?.name ?? cloud.projectSlug} · ${environment?.name ?? cloud.environmentSlug}` : service.identity}
                </ItemDescription>
              </ItemContent>
              <ItemActions>
                {server.status !== "offline" ? (
                  cloud ? <ChevronRightIcon className="size-4 text-muted-foreground" /> : null
                ) : elsewhere ? (
                  <span className="text-sm text-muted-foreground">Still on {elsewhere.name}</span>
                ) : (
                  <ServerStatusLabel status="offline">Down</ServerStatusLabel>
                )}
              </ItemActions>
            </>
          );
          return cloud ? (
            <Item
              key={service.identity}
              variant="outline"
              size="sm"
              render={
                <Link
                  to="/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId"
                  params={{ organizationSlug, projectSlug: cloud.projectSlug, environmentSlug: cloud.environmentSlug, serviceId: cloud.id }}
                />
              }
            >
              {content}
            </Item>
          ) : (
            <Item key={service.identity} variant="outline" size="sm">{content}</Item>
          );
        })}
      </ItemGroup>
    </section>
  );
}
