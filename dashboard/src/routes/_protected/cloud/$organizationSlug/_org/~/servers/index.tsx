import { createFileRoute, Link } from "@tanstack/react-router";
import { ChevronRightIcon, ServerIcon, WifiOffIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import { ServerStatusLabel } from "#/components/server-status-label";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { useServers, type Server } from "#/modules/machines/use-servers";
import { AddServerDialog } from "./-components/add-server-dialog";
import { ServersSkeleton } from "./-components/servers-skeleton";

export const Route = createFileRoute(
  "/_protected/cloud/$organizationSlug/_org/~/servers/",
)({
  component: RouteComponent,
});

function RouteComponent() {
  const { organizationSlug } = Route.useParams();
  const { lens, stale, servers } = useServers(organizationSlug);
  const addServer = <AddServerDialog organizationSlug={organizationSlug} />;

  return (
    <DashboardPage width="content">
      <div className="flex items-center justify-between gap-3">
        <div className="flex min-w-0 flex-col gap-1">
          <h1 className="text-xl font-semibold">Servers</h1>
          {servers.length > 0 && !stale ? <ServersHealth servers={servers} /> : null}
        </div>
        {servers.length > 0 ? addServer : null}
      </div>
      {lens === "connecting" ? (
        <ServersSkeleton />
      ) : servers.length === 0 ? (
        lens === "unreachable" || lens === "unavailable" ? (
          <Empty variant="first-run">
            <EmptyHeader>
              <EmptyMedia variant="icon"><WifiOffIcon /></EmptyMedia>
              <EmptyTitle>Can’t reach your servers right now</EmptyTitle>
              <EmptyDescription>Ployz keeps trying and shows them as soon as it can.</EmptyDescription>
            </EmptyHeader>
          </Empty>
        ) : (
          <Empty variant="first-run">
            <EmptyHeader>
              <EmptyMedia variant="icon"><ServerIcon /></EmptyMedia>
              <EmptyTitle>Add your first server</EmptyTitle>
              <EmptyDescription>Run one command on a Linux server and it joins this organization.</EmptyDescription>
            </EmptyHeader>
            <EmptyContent>{addServer}</EmptyContent>
          </Empty>
        )
      ) : (
        <>
          {stale ? (
            <Alert>
              <WifiOffIcon />
              <AlertTitle>Can’t reach your servers right now</AlertTitle>
              <AlertDescription>Showing what they last reported.</AlertDescription>
            </Alert>
          ) : null}
          <ItemGroup>
            {servers.map((server) => (
              <ServerRow key={server.machine.id} organizationSlug={organizationSlug} server={server} stale={stale} />
            ))}
          </ItemGroup>
        </>
      )}
    </DashboardPage>
  );
}

/** "All 3 online", or the problems first: "1 offline · 2 online". */
function ServersHealth({ servers }: { servers: readonly Server[] }) {
  const count = (status: Server["status"]) => servers.filter((server) => server.status === status).length;
  const offline = count("offline"), slow = count("not_responding"), unknown = count("unknown");
  const online = servers.length - offline - slow - unknown;
  const problems = [offline && `${offline} offline`, slow && `${slow} not responding`, unknown && `${unknown} unknown`].filter(Boolean);
  const text = problems.length === 0
    ? servers.length === 1 ? "Online" : `All ${servers.length} online`
    : [...problems, online > 0 && `${online} online`].filter(Boolean).join(" · ");
  const worst = offline ? "offline" : slow ? "not_responding" : unknown ? "unknown" : "online";
  return <p className="text-sm"><ServerStatusLabel status={worst}>{text}</ServerStatusLabel></p>;
}

function ServerRow({ organizationSlug, server, stale }: { organizationSlug: string; server: Server; stale: boolean }) {
  const names = [...new Set(server.services.map((service) => service.name))];
  return (
    <Item
      variant="outline"
      size="sm"
      render={<Link to="/cloud/$organizationSlug/~/servers/$serverId" params={{ organizationSlug, serverId: server.machine.id }} />}
    >
      <ItemMedia variant="icon"><ServerIcon /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>{server.name}</ItemTitle>
        <ItemDescription className="line-clamp-1">
          {names.length > 0 ? names.join(" · ") : server.machine.acceptsBuilds ? "Only runs builds" : "Nothing running"}
        </ItemDescription>
      </ItemContent>
      <ItemActions>
        <ServerStatusLabel status={server.status} stale={stale} />
        <ChevronRightIcon className="size-4 text-muted-foreground" />
      </ItemActions>
    </Item>
  );
}
