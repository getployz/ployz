import { Link } from "@tanstack/react-router";
import {
  getDashboardDestination,
  type DashboardScope,
} from "./dashboard-navigation-model";
import { DashboardNavigation } from "./dashboard-navigation";
import { NavigationSwitcher } from "./navigation-switcher";
import { PloyzLogo } from "./icons/ployz-logo";
import {
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarSeparator,
  SidebarTrigger,
  useSidebar,
} from "./ui/sidebar";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";
import { cn } from "#/lib/utils";

export function AppSidebar({ scope }: { scope: DashboardScope }) {
  const { open } = useSidebar();
  const home = getDashboardDestination(
    { kind: "all", organizationSlug: scope.organizationSlug },
    "overview",
  );
  return (
    <div className="flex h-full min-h-0 flex-col text-sidebar-foreground">
      <SidebarHeader>
        <div
          className={cn(
            "flex h-12 items-center",
            open ? "justify-between" : "justify-center",
          )}
        >
          {open ? (
            <Link {...home} aria-label="Ployz home">
              <PloyzLogo />
            </Link>
          ) : null}
          <SidebarTrigger
            size="icon"
            aria-label={open ? "Collapse sidebar" : "Expand sidebar"}
            title={open ? "Collapse sidebar" : "Expand sidebar"}
          />
        </div>
        <NavigationSwitcher projection={open ? "desktop" : "rail"} />
      </SidebarHeader>
      <SidebarSeparator />
      <SidebarContent>
        <DashboardNavigation
          scope={scope}
          projection={open ? "desktop" : "rail"}
        />
      </SidebarContent>
      <SidebarFooter>
        <DashboardAccountMenu variant="sidebar" collapsed={!open} />
      </SidebarFooter>
    </div>
  );
}
