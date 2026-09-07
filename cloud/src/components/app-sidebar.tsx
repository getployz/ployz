import { Link } from "@tanstack/react-router";
import {
  createDashboardNavItems,
  getDashboardDestination,
  type DashboardNavItem,
  type DashboardSection,
  type DashboardScope,
} from "#/components/dashboard-navigation-model";
import { PloyzLogo, PloyzMark } from "#/components/icons/ployz-logo";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { cn } from "#/lib/utils";
import {
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarSeparator,
  useSidebar,
} from "#/components/ui/sidebar";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";

type AppSidebarProps = {
  scope: DashboardScope;
  projection?: "desktop" | "mobile";
  onNavigate?: () => void;
};

function NavigationGroup({
  items,
  currentSection,
  open,
  onNavigate,
}: {
  items: DashboardNavItem[];
  currentSection: DashboardSection;
  open: boolean;
  onNavigate?: () => void;
}) {
  return (
    <SidebarGroup>
      <SidebarGroupContent>
        <SidebarMenu>
          {items.map((item) => (
            <SidebarMenuItem key={item.section}>
              <SidebarMenuButton
                className={cn(!open && "justify-center")}
                render={
                  <Link
                    to={item.to}
                    params={item.params}
                    activeOptions={{ exact: true }}
                    aria-label={open ? undefined : item.label}
                    onClick={onNavigate}
                  />
                }
                isActive={item.section === currentSection}
                tooltip={item.label}
              >
                <item.icon />
                {open ? <span>{item.label}</span> : null}
              </SidebarMenuButton>
            </SidebarMenuItem>
          ))}
        </SidebarMenu>
      </SidebarGroupContent>
    </SidebarGroup>
  );
}

export function AppSidebar({
  scope,
  projection = "desktop",
  onNavigate,
}: AppSidebarProps) {
  const { open: sidebarOpen } = useSidebar();
  const open = projection === "mobile" || sidebarOpen;
  const items = createDashboardNavItems(scope);
  const currentSection = useDashboardSection();
  const overviewDestination = getDashboardDestination(
    { kind: "all", organizationSlug: scope.organizationSlug },
    "overview",
  );
  const scopeItems = items.filter((item) => item.group === "scope");
  const organizationItems = items.filter(
    (item) => item.group === "organization",
  );

  return (
    <div className="flex h-full min-h-0 flex-col text-sidebar-foreground">
      <SidebarHeader className="h-(--top-nav-height) shrink-0 p-0">
        <Link
          to={overviewDestination.to}
          params={overviewDestination.params}
          onClick={onNavigate}
          className={cn(
            "flex h-full items-center",
            open ? "px-4" : "justify-center",
          )}
        >
          {open ? <PloyzLogo /> : <PloyzMark className="size-6" />}
        </Link>
      </SidebarHeader>
      <SidebarContent>
        <NavigationGroup
          items={scopeItems}
          currentSection={currentSection}
          open={open}
          onNavigate={onNavigate}
        />
        <SidebarSeparator />
        <NavigationGroup
          items={organizationItems}
          currentSection={currentSection}
          open={open}
          onNavigate={onNavigate}
        />
      </SidebarContent>
      <SidebarFooter>
        <DashboardAccountMenu variant="sidebar" collapsed={!open} />
      </SidebarFooter>
    </div>
  );
}
