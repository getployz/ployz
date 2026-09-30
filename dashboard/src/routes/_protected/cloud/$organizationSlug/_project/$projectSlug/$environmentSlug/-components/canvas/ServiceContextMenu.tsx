import type { ReactElement } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { BracesIcon, SettingsIcon, Trash2Icon } from "lucide-react";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuGroup,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "#/components/ui/context-menu";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

const tabs = [
  { tab: "settings", label: "View settings", icon: SettingsIcon },
  { tab: "variables", label: "View variables", icon: BracesIcon },
] as const;

export function ServiceContextMenu({
  serviceId,
  onDelete,
  children,
}: {
  serviceId: string;
  onDelete: () => void;
  children: ReactElement;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });

  return (
    <ContextMenu>
      <ContextMenuTrigger render={children} />
      <ContextMenuContent>
        <ContextMenuGroup>
          {tabs.map(({ tab, label, icon: Icon }) => (
            <ContextMenuItem
              key={tab}
              render={
                <Link
                  to={ENVIRONMENT_SERVICE_ROUTE_TO}
                  params={{ ...params, serviceId }}
                  search={(prev) => ({ ...prev, tab })}
                />
              }
            >
              <Icon />
              {label}
            </ContextMenuItem>
          ))}
        </ContextMenuGroup>
        <ContextMenuSeparator />
        <ContextMenuGroup>
          <ContextMenuItem variant="destructive" onClick={onDelete}>
            <Trash2Icon />
            Delete service
          </ContextMenuItem>
        </ContextMenuGroup>
      </ContextMenuContent>
    </ContextMenu>
  );
}
