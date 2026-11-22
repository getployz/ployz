import { useState, type ReactNode } from "react";
import { getRouteApi, Link } from "@tanstack/react-router";
import { BoxIcon, ChevronRightIcon } from "lucide-react";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsFoot, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "#/components/ui/sheet";
import type { Server } from "#/modules/machines/use-servers";
import { runsHere } from "./runs-here";
import { offlineCover } from "./server-page";
import { ServerStatusLabel } from "./server-status-label";
import { ServiceUsage } from "./service-usage";
import { StrayNamespaces } from "./stray-namespaces";

const serverRoute = getRouteApi("/_protected/cloud/$organizationSlug/_org/~/servers/$serverId");

/** Rows shown in place; each reads its Service's usage, so the rest wait in a sheet that reads only while open. */
const SHOWN_HERE = 5;

/**
 * The Services on this Server with what each uses here now. While the Server is offline, each row says whether its
 * Service still runs elsewhere, and the sheet link counts those that run nowhere else.
 */
export function RunningHere({ organizationSlug, server, servers, stale }: {
  organizationSlug: string;
  server: Server;
  servers: readonly Server[];
  stale: boolean;
}) {
  const open = serverRoute.useSearch().services === true;
  const navigate = serverRoute.useNavigate();
  const { up, onlyHere } = offlineCover(server, servers);
  const count = server.services.length;
  const row = (service: Server["services"][number]) => (
    <ServiceRow key={service.identity} organizationSlug={organizationSlug} server={server} service={service} up={up} stale={stale} />
  );

  return (
    <>
      <SettingsSection id="running-here" title="Running here">
        {count === 0 ? <SettingsEmpty title={runsHere(server)} /> : (
          <SettingsList label={`Services on ${server.name}`}>{server.services.slice(0, SHOWN_HERE).map(row)}</SettingsList>
        )}
        <StrayNamespaces organizationSlug={organizationSlug} server={server} servers={servers} />
        {count > SHOWN_HERE ? (
          <SettingsFoot>
            <Button variant="outline" size="sm" render={<Link from="/cloud/$organizationSlug/~/servers/$serverId" to="." search={{ services: true }} />}>
              All {count} services
              <ChevronRightIcon data-icon="inline-end" />
            </Button>
            {onlyHere > 0 ? <ServerStatusLabel status="offline" stale={stale}>{onlyHere} run only here</ServerStatusLabel> : null}
          </SettingsFoot>
        ) : null}
      </SettingsSection>
      <Sheet open={open && count > SHOWN_HERE} onOpenChange={(next) => { if (!next) void navigate({ to: ".", search: {} }); }}>
        <SheetContent className="gap-0">
          <SheetHeader>
            <SheetTitle>Running on {server.name}</SheetTitle>
          </SheetHeader>
          <ServiceList services={server.services} row={row} />
        </SheetContent>
      </Sheet>
    </>
  );
}

function ServiceList({ services, row }: {
  services: Server["services"];
  row: (service: Server["services"][number]) => ReactNode;
}) {
  const [filter, setFilter] = useState("");
  const needle = filter.trim().toLowerCase();
  const shown = needle
    ? services.filter((service) => `${service.name} ${service.namespace ?? ""}`.toLowerCase().includes(needle))
    : services;

  return (
    <>
      <div className="px-4 pb-3">
        <Input
          type="search"
          aria-label="Filter services"
          placeholder="Filter services"
          value={filter}
          onChange={(event) => setFilter(event.target.value)}
        />
      </div>
      <div className="@container/settings mx-4 mb-4 min-h-0 overflow-y-auto rounded-lg border bg-card">
        {shown.length === 0 ? <SettingsEmpty title={`No services match “${filter.trim()}”`} /> : (
          <SettingsList label="Services">{shown.map(row)}</SettingsList>
        )}
      </div>
    </>
  );
}

function ServiceRow({ organizationSlug, server, service, up, stale }: {
  organizationSlug: string;
  server: Server;
  service: Server["services"][number];
  up: readonly Server[];
  stale: boolean;
}) {
  const elsewhere = up.find((other) => service.machineIds.has(other.machine.id));
  return (
    <SettingsListItem
      media={<BoxIcon />}
      label={service.name}
      hint={service.namespace ? <span className="line-clamp-1">{service.namespace}</span> : undefined}
      commit="none"
      actions={server.status !== "offline" ? (
        service.namespace ? (
          <ServiceUsage
            organizationSlug={organizationSlug}
            server={{ id: server.machine.id, name: server.name }}
            namespace={service.namespace}
            name={service.name}
          />
        ) : null
      ) : elsewhere ? (
        <span className="text-sm text-muted-foreground">Still on {elsewhere.name}</span>
      ) : (
        <ServerStatusLabel status="offline" stale={stale}>Down</ServerStatusLabel>
      )}
    />
  );
}
