import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { ChevronRightIcon, ServerIcon } from "lucide-react";
import { Item, ItemActions, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import type { ServerListItem } from "#/modules/machines/use-servers";
import { volumeSupportText } from "#/modules/machines/server-status";

/** One Server as a row that opens its page. */
export function ServerLinkItem({ organizationSlug, server, description, children }: {
  organizationSlug: string;
  server: ServerListItem;
  description: ReactNode;
  children?: ReactNode;
}) {
  return (
    <Item
      variant="outline"
      size="sm"
      render={<Link to="/cloud/$organizationSlug/~/servers/$serverId" params={{ organizationSlug, serverId: server.machine.id }} />}
    >
      <ItemMedia variant="icon"><ServerIcon /></ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>{server.name}</ItemTitle>
        <ItemDescription className="line-clamp-1">{description}</ItemDescription>
        <ItemDescription>{volumeSupportText(server.machine.storage)}</ItemDescription>
      </ItemContent>
      <ItemActions>
        {children}
        <ChevronRightIcon className="size-4 text-muted-foreground" />
      </ItemActions>
    </Item>
  );
}
