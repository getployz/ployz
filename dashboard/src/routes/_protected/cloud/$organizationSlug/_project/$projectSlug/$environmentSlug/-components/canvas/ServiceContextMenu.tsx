import type { ReactElement } from "react";
import { Link, useParams } from "@tanstack/react-router";
import { BracesIcon, HistoryIcon, SettingsIcon, Trash2Icon } from "lucide-react";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuGroup,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from "#/components/ui/context-menu";
import { useDeleteService } from "../../services/$serviceId/-components/useDeleteService";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_SERVICE_ROUTE_TO } from "../environment-route-paths";

const tabs = [
  { tab: "settings", label: "View settings", icon: SettingsIcon },
  { tab: "variables", label: "View variables", icon: BracesIcon },
  { tab: "deployments", label: "View deployments", icon: HistoryIcon },
] as const;

export function ServiceContextMenu({
  serviceId,
  children,
}: {
  serviceId: string;
  children: ReactElement;
}) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const deleteService = useDeleteService(serviceId);

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
          <ContextMenuItem variant="destructive" onClick={() => void deleteService()}>
            <Trash2Icon />
            Delete service
          </ContextMenuItem>
        </ContextMenuGroup>
      </ContextMenuContent>
    </ContextMenu>
  );
}
