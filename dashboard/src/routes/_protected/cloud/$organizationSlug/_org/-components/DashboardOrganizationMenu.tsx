import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { ArrowLeftRightIcon, ChevronsUpDownIcon, CreditCardIcon, PlusIcon, SlidersHorizontalIcon } from "lucide-react";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { cn } from "#/lib/utils";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";
import { NewOrganizationDialog } from "./new-organization-dialog";
import { OrganizationSwitchItems } from "./organization-switch-items";

/**
 * Prototype F: the organization behind its name atop the rail. Its menu opens Settings and Billing, and switches or
 * creates an organization; while Settings or Billing is open, the name is the current place. `narrow` leaves its mark.
 */
export function DashboardOrganizationMenu({ scope, narrow = false, className }: {
  scope: DashboardScope;
  narrow?: boolean;
  className?: string;
}) {
  const section = useDashboardSection();
  const [creating, setCreating] = useState(false);
  const state = useQuery(organizationStateQueryOptions(scope.organizationSlug)).data;
  const name = state?.organizations.find((candidate) => candidate.slug === scope.organizationSlug)?.name ?? scope.organizationSlug;
  const current = section === "organization-settings" || section === "billing";
  const params = { organizationSlug: scope.organizationSlug };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger aria-label={`${name}, organization menu`} title={narrow ? name : undefined}
        aria-current={current ? "page" : undefined}
        render={<button type="button" className={cn(
          "flex h-9 shrink-0 items-center gap-2.5 rounded-md px-2.5 text-left text-sm font-medium outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50 aria-expanded:bg-muted aria-[current=page]:bg-muted",
          narrow ? "justify-center px-0" : "w-full min-w-0",
          className,
        )} />}>
        {/* Its mark sits where the places' icons do, so it lines up with them in both widths. */}
        <span aria-hidden className="flex size-5 shrink-0 items-center justify-center rounded bg-foreground text-[11px] font-semibold text-background uppercase">
          {name.slice(0, 1)}
        </span>
        {narrow ? null : (
          <>
            <span className="min-w-0 flex-1 truncate">{name}</span>
            <ChevronsUpDownIcon className="size-4 shrink-0 text-muted-foreground" aria-hidden />
          </>
        )}
      </DropdownMenuTrigger>
      <DropdownMenuContent side={narrow ? "right" : "bottom"} align="start" className="w-auto min-w-56">
        <DropdownMenuGroup>
          <DropdownMenuItem render={<Link to="/cloud/$organizationSlug/~/settings" params={params} search={{ variant: "f" }} />}>
            <SlidersHorizontalIcon />
            Settings
          </DropdownMenuItem>
          {/* Only Ployz-hosted Cloud has billing. */}
          {state?.billingEnabled ? (
            <DropdownMenuItem render={<Link to="/cloud/$organizationSlug/~/billing" params={params} search={{ variant: "f" }} />}>
              <CreditCardIcon />
              Billing
            </DropdownMenuItem>
          ) : null}
        </DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuGroup>
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <ArrowLeftRightIcon />
              Switch organization
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent>
              <OrganizationSwitchItems scope={scope} />
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuItem onClick={() => setCreating(true)}>
            <PlusIcon />
            New organization
          </DropdownMenuItem>
        </DropdownMenuGroup>
      </DropdownMenuContent>
      {creating && <NewOrganizationDialog onOpenChange={setCreating} />}
    </DropdownMenu>
  );
}
