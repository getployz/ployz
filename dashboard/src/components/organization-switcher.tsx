import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Building2Icon, ChevronsUpDownIcon } from "lucide-react";
import { getDashboardDestination } from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { Button } from "#/components/ui/button";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "#/components/ui/popover";
import { Command, CommandGroup, CommandItem, CommandList } from "#/components/ui/command";
import { Skeleton } from "#/components/ui/skeleton";
import { cn } from "#/lib/utils";
import { organizationStateQueryOptions } from "#/modules/environment-design/workspace.queries";

type Projection = "desktop" | "mobile" | "rail";

export function OrganizationSwitcher({
  projection = "desktop",
}: {
  projection?: Projection;
}) {
  const [organizationOpen, setOrganizationOpen] = useState(false);
  const navigate = useNavigate();
  const rail = projection === "rail";
  const { organizationSlug } = useParams({
    strict: false,
  });
  const section = useDashboardSection();
  const {
    data: organizationState,
    isPending,
    isError,
    refetch,
  } = useQuery(organizationStateQueryOptions(organizationSlug));
  if (!organizationSlug) return null;
  const organizationLabel =
    organizationState?.activeOrganization?.name ?? organizationSlug;

  return (
    <Popover open={organizationOpen} onOpenChange={setOrganizationOpen}>
      <PopoverTrigger openOnHover={rail} render={
        <Button variant="ghost" size={rail ? "icon" : projection === "mobile" ? "sm" : "default"}
          aria-label={`Organization: ${organizationLabel}`} title={organizationLabel}
          className={cn(!rail && "w-full min-w-0 justify-start")} />
      }>
        {projection !== "mobile" && <Building2Icon data-icon="inline-start" />}
        {!rail && <>
          <span className="min-w-0 flex-1 truncate text-left">{organizationLabel}</span>
          <ChevronsUpDownIcon data-icon="inline-end" />
        </>}
      </PopoverTrigger>
      <PopoverContent padding="none" align="start" side={rail ? "right" : "bottom"}
        className="w-[min(18rem,calc(100vw-2rem))]">
        <PopoverTitle className="sr-only">Choose organization</PopoverTitle>
        <div className="min-w-0">
          <Command tabIndex={0} label="Organizations">
            <CommandList className="max-h-[min(20rem,45dvh)]">
              <CommandGroup heading="Organizations">
                {organizationState?.organizations.map((organization) => (
                  <CommandItem key={organization.id} value={organization.slug}
                    data-checked={organization.slug === organizationSlug}
                    onSelect={() => {
                      setOrganizationOpen(false);
                      void navigate(getDashboardDestination(
                        { kind: "all", organizationSlug: organization.slug }, section,
                      ));
                    }}>
                    <Building2Icon /><span className="truncate">{organization.name}</span>
                  </CommandItem>
                ))}
                {isPending ? (
                  <div role="status" aria-label="Loading organizations" className="p-1"><Skeleton className="h-7 w-full" /></div>
                ) : isError ? (
                  <CommandItem onSelect={() => void refetch()}>Could not load organizations. Retry</CommandItem>
                ) : null}
              </CommandGroup>
            </CommandList>
          </Command>
        </div>
      </PopoverContent>
    </Popover>
  );
}
