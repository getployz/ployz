import { createFileRoute } from "@tanstack/react-router";
import { ServerIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { ServerStatusLabel, serverStatusWord } from "#/components/server-status-label";
import { Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyTitle } from "#/components/ui/empty";
import { ItemGroup } from "#/components/ui/item";
import { needsAttention, type ServerStatus } from "#/modules/machines/server-status";
import { useServers, type Server } from "#/modules/machines/use-servers";
import { ServerLinkItem } from "#/routes/_protected/cloud/$organizationSlug/_org/-components/server-link-item";
import { AddServerDialog } from "./-components/add-server-dialog";
import { runsHere } from "./-components/runs-here";
import { ServersSkeleton } from "./-components/servers-skeleton";
import { ServersStaleAlert, ServersUnreachable } from "./-components/servers-unreachable";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { state, servers } = useServers(organizationSlug);

  return (
    <DashboardPage width="content">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 flex-col gap-1">
          <h1 className="text-xl font-semibold">Servers</h1>
          {state === "live" && servers.length > 0 ? <ServersHealth servers={servers} /> : null}
        </div>
        <AddServerDialog organizationSlug={organizationSlug} />
      </div>
      {state === "loading" ? (
        <ServersSkeleton />
      ) : state === "unreachable" ? (
        <ServersUnreachable />
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
          <ItemGroup>
            {servers.map((server) => (
              <ServerLinkItem key={server.machine.id} organizationSlug={organizationSlug} server={server} description={runsHere(server)}>
                <ServerStatusLabel status={server.status} stale={state === "stale"} />
              </ServerLinkItem>
            ))}
          </ItemGroup>
        </>
      )}
    </DashboardPage>
  );
}

/** "All 3 online", or what needs someone first: "1 offline · 2 online". */
function ServersHealth({ servers }: { servers: readonly Server[] }) {
  // Servers arrive sorted, so problems count in the order they rank.
  const problems = new Map<ServerStatus, number>();
  for (const { status } of servers) if (needsAttention(status)) problems.set(status, (problems.get(status) ?? 0) + 1);
  const online = servers.length - [...problems.values()].reduce((sum, count) => sum + count, 0);
  const text = problems.size === 0
    ? servers.length === 1 ? "Online" : `All ${servers.length} online`
    : [...[...problems].map(([status, count]) => `${count} ${serverStatusWord(status).toLowerCase()}`), ...(online > 0 ? [`${online} online`] : [])].join(" · ");
  return <p className="text-sm"><ServerStatusLabel status={problems.keys().next().value ?? "online"}>{text}</ServerStatusLabel></p>;
}
