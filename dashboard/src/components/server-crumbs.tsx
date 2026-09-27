import { useState } from "react";
import { Link, useNavigate } from "@tanstack/react-router";
import { ChevronsUpDownIcon } from "lucide-react";
import { ServerStatusLabel } from "#/components/server-status-label";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbSeparator,
} from "#/components/ui/breadcrumb";
import { Button } from "#/components/ui/button";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "#/components/ui/command";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { sortServers, useServerStatuses } from "#/modules/machines/server-status";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";

/** Servers / hel-1 ⌄ — the crumb back to the list, and a switcher over the other Servers. Chrome: reads only the Runtime. */
export function ServerCrumbs({ organizationSlug, serverId }: { organizationSlug: string; serverId: string }) {
  const runtime = useRuntimeLens(organizationSlug);
  const statusOf = useServerStatuses(runtime.machines);
  const servers = sortServers(runtime.machines.map((machine) => ({ id: machine.id, name: machine.name, status: statusOf(machine) })));
  const current = servers.find((server) => server.id === serverId);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);

  return (
    <Breadcrumb className="min-w-0">
      <BreadcrumbList className="flex-nowrap">
        <BreadcrumbItem>
          <BreadcrumbLink render={<Link to="/cloud/$organizationSlug/~/servers" params={{ organizationSlug }} />}>Servers</BreadcrumbLink>
        </BreadcrumbItem>
        <BreadcrumbSeparator />
        <BreadcrumbItem className="min-w-0">
          <Popover open={open} onOpenChange={setOpen}>
            <PopoverTrigger render={<Button variant="ghost" size="sm" className="min-w-0" aria-label={`Server: ${current?.name ?? serverId}`} />}>
              <span className="truncate font-semibold text-foreground">{current?.name ?? serverId}</span>
              <ChevronsUpDownIcon data-icon="inline-end" />
            </PopoverTrigger>
            <PopoverContent padding="none" align="start" className="w-72">
              <PopoverTitle className="sr-only">Switch server</PopoverTitle>
              <Command label="Servers" value={serverId}>
                {servers.length > 8 ? <CommandInput placeholder="Find a server…" /> : null}
                <CommandList>
                  <CommandEmpty>No servers found</CommandEmpty>
                  <CommandGroup>
                    {servers.map((server) => (
                      <CommandItem
                        key={server.id}
                        value={server.id}
                        keywords={[server.name]}
                        data-checked={server.id === serverId}
                        onSelect={() => {
                          setOpen(false);
                          void navigate({ to: "/cloud/$organizationSlug/~/servers/$serverId", params: { organizationSlug, serverId: server.id } });
                        }}
                      >
                        <span className="min-w-0 flex-1 truncate">{server.name}</span>
                        <ServerStatusLabel status={server.status} stale={runtime.status === "unavailable"} />
                      </CommandItem>
                    ))}
                  </CommandGroup>
                </CommandList>
              </Command>
            </PopoverContent>
          </Popover>
        </BreadcrumbItem>
      </BreadcrumbList>
    </Breadcrumb>
  );
}
