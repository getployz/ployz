import { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { BoxIcon, DatabaseIcon, SearchIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  Command,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "#/components/ui/command";
import { Kbd } from "#/components/ui/kbd";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { nodeDestination, type NavigationNode } from "../environment-node-navigation";

const nodeIcons = { service: BoxIcon, volume: DatabaseIcon };

/** Find: the button and `/` open a finder over the Environment's services and volumes; choosing one opens its panel. */
export function CanvasFinder({ nodes }: { nodes: NavigationNode[] }) {
  const [open, setOpen] = useState(false);
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const navigate = useNavigate();

  useEffect(() => {
    function openOnSlash(event: KeyboardEvent) {
      if (event.key !== "/" || event.altKey || event.ctrlKey || event.metaKey || event.repeat || event.defaultPrevented) return;
      // `/` types a slash in fields.
      const target = event.target;
      if (target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement ||
        (target instanceof HTMLElement && target.isContentEditable)) return;
      event.preventDefault();
      setOpen(true);
    }
    document.addEventListener("keydown", openOnSlash);
    return () => document.removeEventListener("keydown", openOnSlash);
  }, []);

  return (
    <>
      <Button variant="outline" className="pointer-events-auto" onClick={() => setOpen(true)}>
        <SearchIcon data-icon="inline-start" />
        Find
        <Kbd>/</Kbd>
      </Button>
      <CommandDialog open={open} onOpenChange={setOpen} title="Find a resource"
        description="Open a service or volume" className="max-w-md">
        <Command label="Resources">
          <CommandInput placeholder="Find a service or volume…" />
          <CommandList>
            <CommandEmpty>No matching resources</CommandEmpty>
            <CommandGroup>
              {nodes.map((node) => {
                const Icon = nodeIcons[node.type];
                return (
                  <CommandItem key={node.id} value={`${node.name} ${node.id}`} onSelect={() => {
                    setOpen(false);
                    void navigate(nodeDestination(params, node));
                  }}>
                    <Icon />{node.name}
                  </CommandItem>
                );
              })}
            </CommandGroup>
          </CommandList>
        </Command>
      </CommandDialog>
    </>
  );
}
