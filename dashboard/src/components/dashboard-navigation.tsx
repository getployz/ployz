import { useState } from "react";
import { Link, useSearch } from "@tanstack/react-router";
import {
  ArrowLeftIcon,
  BoxIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  DatabaseIcon,
} from "lucide-react";
import {
  createDashboardNavItems,
  getDashboardDestination,
  getDashboardSectionLabel,
  type DashboardScope,
  type DashboardNavItem,
} from "./dashboard-navigation-model";
import { useDashboardSection } from "./use-dashboard-section";
import { NavigationSwitcher } from "./navigation-switcher";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Empty, EmptyDescription } from "./ui/empty";
import { Skeleton } from "./ui/skeleton";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "./ui/collapsible";
import {
  Popover,
  PopoverContent,
  PopoverTitle,
  PopoverTrigger,
} from "./ui/popover";
import {
  SidebarGroup,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuItem,
  SidebarMenuButton,
} from "./ui/sidebar";
import { cn } from "#/lib/utils";
import { useCanvasInspectorSelection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/useCanvasInspectorSelection";
import {
  nodeDestination,
  useEnvironmentNavigationNodes,
  type NavigationNode,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-node-navigation";
import { SERVICE_PAGES } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/service-pages";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";

type EnvironmentScope = Extract<DashboardScope, { kind: "environment" }>;
type Projection = "desktop" | "rail" | "mobile";
const nodeIcons = {
  service: BoxIcon,
  volume: DatabaseIcon,
};

function Destination({
  item,
  current,
  rail,
  onNavigate,
}: {
  item: DashboardNavItem;
  current: boolean;
  rail: boolean;
  onNavigate?: () => void;
}) {
  return (
    <SidebarMenuItem>
      <SidebarMenuButton
        render={
          <Link
            to={item.to}
            params={item.params}
            search={item.search}
            onClick={onNavigate}
            aria-current={current ? "page" : undefined}
          />
        }
        isActive={current}
        tooltip={item.label}
        aria-label={item.label}
        className={cn(rail && "justify-center")}
      >
        <item.icon />
        {!rail ? <span>{item.label}</span> : null}
      </SidebarMenuButton>
    </SidebarMenuItem>
  );
}

function ResourceNavigation({
  scope, node, selected, tab, onNavigate,
}: {
  scope: EnvironmentScope;
  node: NavigationNode;
  selected: boolean;
  tab?: string;
  onNavigate?: (nodeId: string) => void;
}) {
  const [open, setOpen] = useState(selected);
  const Icon = nodeIcons[node.type];
  const pages = node.type === "service" ? SERVICE_PAGES : [SERVICE_PAGES[0]];
  return (
    <SidebarMenuItem>
      <Collapsible open={open} onOpenChange={setOpen}>
        <div className="flex min-w-0 items-center">
          <SidebarMenuButton render={<Link {...nodeDestination(scope, node)}
            onClick={() => onNavigate?.(node.id)} title={node.name} />}>
            <Icon /><span>{node.name}</span>
          </SidebarMenuButton>
          <CollapsibleTrigger render={<Button variant="ghost" size="icon"
            aria-label={`${open ? "Collapse" : "Expand"} ${node.name}`} />}>
            <ChevronRightIcon className={cn(open && "rotate-90")} />
          </CollapsibleTrigger>
        </div>
        <CollapsibleContent>
          <div className="pl-4">
          <SidebarMenu>
            {pages.map((page) => (
              <SidebarMenuItem key={page.id}>
                <SidebarMenuButton isActive={selected && (tab ?? "settings") === page.id}
                  render={<Link {...nodeDestination(scope, node, page.id)}
                    onClick={() => onNavigate?.(node.id)}
                    aria-current={selected && (tab ?? "settings") === page.id ? "page" : undefined} />}>
                  <span>{page.label}</span>
                </SidebarMenuButton>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
          </div>
        </CollapsibleContent>
      </Collapsible>
    </SidebarMenuItem>
  );
}

export function EnvironmentNodeDirectory({
  scope, nodes, selectedId, onNavigate,
}: {
  scope: EnvironmentScope;
  nodes: NavigationNode[];
  selectedId: string | null;
  onNavigate?: (nodeId: string) => void;
}) {
  const [filter, setFilter] = useState("");
  const { tab } = useSearch({ strict: false });
  const selected = nodes.find((node) => node.id === selectedId);
  const others = nodes.filter((node) => node.id !== selectedId &&
    node.name.toLowerCase().includes(filter.trim().toLowerCase()));
  return (
    <div className="flex min-w-0 flex-col gap-1">
      {nodes.length > 6 || filter !== "" ? (
        <Input type="search" aria-label="Find resource" placeholder="Find resource…"
          value={filter} onChange={(event) => setFilter(event.target.value)} />
      ) : null}
      {selected ? (
        <SidebarMenu>
          <ResourceNavigation key={selected.id} scope={scope} node={selected} selected tab={tab} onNavigate={onNavigate} />
        </SidebarMenu>
      ) : null}
      <SidebarMenu className="max-h-64 overflow-y-auto overscroll-contain">
        {others.map((node) => (
          <ResourceNavigation key={node.id} scope={scope} node={node} selected={false} tab={tab} onNavigate={onNavigate} />
        ))}
      </SidebarMenu>
      {!others.length && (!selected || filter) ? (
        <Empty variant="placeholder">
          <EmptyDescription>{filter ? "No matching resources" : "No resources yet"}</EmptyDescription>
        </Empty>
      ) : null}
    </div>
  );
}

function EnvironmentNavigation({
  scope,
  projection,
  onNavigate,
}: {
  scope: EnvironmentScope;
  projection: Projection;
  onNavigate?: () => void;
}) {
  const { selectedNodeId } = useCanvasInspectorSelection();
  const [disclosure, setDisclosure] = useState<{
    selectedNodeId: string | null;
    projection: Projection;
    open: boolean;
    userOpened: boolean;
    navigationTarget: string | null;
  }>({
    selectedNodeId,
    projection,
    open: projection !== "rail" && selectedNodeId !== null,
    userOpened: false,
    navigationTarget: null,
  });
  if (disclosure.selectedNodeId !== selectedNodeId || disclosure.projection !== projection) {
    setDisclosure({
      selectedNodeId,
      projection,
      open:
        disclosure.projection !== projection
          ? projection !== "rail" && (selectedNodeId !== null || disclosure.open)
          : selectedNodeId === null
            ? disclosure.open
            : projection !== "rail" && selectedNodeId !== disclosure.navigationTarget,
      userOpened: false,
      navigationTarget: null,
    });
  }
  const { open } = disclosure;
  const setOpen = (value: boolean, details?: { reason: string }) =>
    setDisclosure((current) => ({
      ...current,
      open: value,
      userOpened: details?.reason !== "trigger-hover" && (value || current.userOpened),
      navigationTarget: value ? null : current.navigationTarget,
    }));
  const navigateToNode = (nodeId: string) => {
    if (projection === "rail") {
      // The arriving selection must not reopen the popover the user just left.
      setDisclosure((current) => ({
        ...current,
        open: false,
        userOpened: false,
        navigationTarget: nodeId,
      }));
    }
    onNavigate?.();
  };
  const resources = useEnvironmentNavigationNodes(scope);
  const section = useDashboardSection();
  const items = createDashboardNavItems(scope);
  const architecture = items.find((item) => item.section === "overview");
  const organization = getDashboardDestination(
    { kind: "all", organizationSlug: scope.organizationSlug },
    "overview",
  );
  const directory = resources.isError ? (
    <Empty variant="placeholder">
      <EmptyDescription>Could not load resources.</EmptyDescription>
      <Button variant="ghost" onClick={resources.retry}>
        Retry
      </Button>
    </Empty>
  ) : resources.isLoading ? (
    <div role="status" aria-label="Loading resources">
      <Skeleton className="h-8 w-full" />
    </div>
  ) : (
    <EnvironmentNodeDirectory
      scope={scope}
      nodes={resources.nodes}
      selectedId={selectedNodeId}
      onNavigate={navigateToNode}
    />
  );

  if (!architecture) return null;
  return (
    <SidebarGroup>
      <SidebarMenu className="mb-1">
        <SidebarMenuItem>
          <SidebarMenuButton
            render={<Link {...organization} onClick={onNavigate} />}
            tooltip="Organization"
            aria-label="Organization"
            className={cn(projection === "rail" && "justify-center")}
          >
            <ArrowLeftIcon />
            {projection !== "rail" ? <span>Organization</span> : null}
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
      {projection !== "rail" ? (
        <SidebarGroupLabel>Project</SidebarGroupLabel>
      ) : null}
      <SidebarMenu>
        <SidebarMenuItem>
          {projection === "rail" ? (
            <Popover open={open} onOpenChange={(value, details) => {
              if (details.reason === "trigger-press") return;
              setOpen(value, details);
            }}>
              <PopoverTrigger
                openOnHover
                nativeButton={false}
                role="link"
                render={
                  <SidebarMenuButton
                    render={<Link {...getDashboardDestination(scope, "overview")}
                      onClick={() => {
                        setOpen(false);
                        onNavigate?.();
                      }} />}
                    aria-label="Architecture"
                    isActive={section === "overview"}
                    className="justify-center"
                  />
                }
              >
                <architecture.icon />
              </PopoverTrigger>
              <PopoverContent
                side="right"
                sideOffset={8}
                align="start"
                initialFocus={() => disclosure.userOpened}
                finalFocus={() => disclosure.userOpened}
              >
                <PopoverTitle className="sr-only">Architecture</PopoverTitle>
                <SidebarMenuButton
                  render={
                    <Link
                      {...getDashboardDestination(scope, "overview")}
                      onClick={() => {
                        setDisclosure((current) => ({
                          ...current,
                          open: false,
                          userOpened: false,
                        }));
                        onNavigate?.();
                      }}
                    />
                  }
                >
                  Architecture
                </SidebarMenuButton>
                {directory}
              </PopoverContent>
            </Popover>
          ) : (
            <Collapsible open={open} onOpenChange={setOpen}>
              <div className="flex items-center">
                <SidebarMenuButton
                  isActive={section === "overview" && !selectedNodeId}
                  render={
                    <Link
                      {...getDashboardDestination(scope, "overview")}
                      onClick={onNavigate}
                      aria-current={
                        section === "overview" && !selectedNodeId
                          ? "page"
                          : undefined
                      }
                    />
                  }
                >
                  <architecture.icon />
                  <span>Architecture</span>
                </SidebarMenuButton>
                <CollapsibleTrigger
                  render={
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={
                        open ? "Collapse Architecture" : "Expand Architecture"
                      }
                    />
                  }
                >
                  <ChevronRightIcon className={cn(open && "rotate-90")} />
                </CollapsibleTrigger>
              </div>
              <CollapsibleContent>
                <div className="pl-4">{directory}</div>
              </CollapsibleContent>
            </Collapsible>
          )}
        </SidebarMenuItem>
        {items
          .filter((item) => item.section !== "overview")
          .map((item) => (
            <Destination
              key={item.section}
              item={item}
              current={section === item.section}
              rail={projection === "rail"}
              onNavigate={onNavigate}
            />
          ))}
      </SidebarMenu>
    </SidebarGroup>
  );
}

export function DashboardNavigation({
  scope,
  projection = "desktop",
  onNavigate,
}: {
  scope: DashboardScope;
  projection?: Projection;
  onNavigate?: () => void;
}) {
  const section = useDashboardSection();
  if (scope.kind === "environment") {
    return (
      <EnvironmentNavigation
        key={`${scope.organizationSlug}/${scope.projectSlug}/${scope.environmentSlug}`}
        scope={scope}
        projection={projection}
        onNavigate={onNavigate}
      />
    );
  }
  return (
    <SidebarGroup>
      {projection !== "rail" ? (
        <SidebarGroupLabel>Organization</SidebarGroupLabel>
      ) : null}
      <SidebarMenu>
        {createDashboardNavItems(scope).map((item) => (
          <Destination
            key={item.section}
            item={item}
            current={section === item.section}
            rail={projection === "rail"}
            onNavigate={onNavigate}
          />
        ))}
      </SidebarMenu>
    </SidebarGroup>
  );
}

export function DashboardNavigationPicker({
  scope,
}: {
  scope: DashboardScope;
}) {
  const [open, setOpen] = useState(false);
  const section = useDashboardSection();
  const { isInspectorOpen, selectedServiceId } = useCanvasInspectorSelection();
  const { tab } = useSearch({ strict: false });
  const navigationLabel = scope.kind === "all" ? "Organization navigation" : "Project navigation";
  const title = isInspectorOpen
    ? selectedServiceId
      ? (SERVICE_PAGES.find((page) => page.id === (tab ?? "settings"))?.label ??
        "Settings")
      : "Settings"
    : getDashboardSectionLabel(scope, section);
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        render={
          <Button
            variant={isInspectorOpen ? "ghost" : "outline"}
            className="min-w-0 flex-1 justify-between"
            aria-label={navigationLabel}
          />
        }
      >
        <span className="truncate">{title}</span>
        <ChevronDownIcon data-icon="inline-end" />
      </PopoverTrigger>
      <PopoverContent
        align="start"
        className="max-h-[70dvh] w-[min(24rem,calc(100vw-1.5rem))] overflow-y-auto"
      >
        <PopoverTitle className="sr-only">{navigationLabel}</PopoverTitle>
        <DashboardNavigation
          scope={scope}
          projection="mobile"
          onNavigate={() => setOpen(false)}
        />
      </PopoverContent>
    </Popover>
  );
}

export function MobileDashboardNavigation({
  scope,
}: {
  scope: DashboardScope;
}) {
  const { isInspectorOpen } = useCanvasInspectorSelection();
  return (
    <div className="flex shrink-0 flex-col gap-2 border-b p-3 min-wf-nav:hidden">
      <div className="flex min-w-0 items-center gap-2">
        <div className="min-w-0 flex-1">
          <NavigationSwitcher projection="mobile" />
        </div>
        <DashboardAccountMenu />
      </div>
      {!isInspectorOpen ? (
        <div className="flex min-w-0">
          <DashboardNavigationPicker scope={scope} />
        </div>
      ) : null}
    </div>
  );
}
