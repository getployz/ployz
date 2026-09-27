import { useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ChevronsUpDownIcon } from "lucide-react";
import { ServerStatusLabel } from "#/components/server-status-label";
import { Button } from "#/components/ui/button";
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "#/components/ui/command";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { useServerList } from "#/modules/machines/use-servers";

/** The server page's crumb: its name, opening a switcher over every Server. Renders in the header, above the Org Store gate. */
export function ServerSwitcher() {
  const { organizationSlug, serverId } = useParams({ from: "/_protected/cloud/$organizationSlug/_org/~/servers/$serverId" });
  const { state, servers } = useServerList(organizationSlug);
  const current = servers.find((server) => server.machine.id === serverId);
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);

  return (
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
                  key={server.machine.id}
                  value={server.machine.id}
                  keywords={[server.name]}
                  data-checked={server.machine.id === serverId}
                  onSelect={() => {
                    setOpen(false);
                    void navigate({ to: "/cloud/$organizationSlug/~/servers/$serverId", params: { organizationSlug, serverId: server.machine.id } });
                  }}
                >
                  <span className="min-w-0 flex-1 truncate">{server.name}</span>
                  <ServerStatusLabel status={server.status} stale={state === "stale"} />
                </CommandItem>
              ))}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
