import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import {
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
} from "#/components/ui/dropdown-menu";
import { getDashboardDestination, type DashboardScope } from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";

/** Your Organizations to switch to, the same page open in each; Ployz support sees the ones that allow it apart. */
export function OrganizationSwitchItems({ scope }: { scope: DashboardScope }) {
  const navigate = useNavigate();
  const section = useDashboardSection();
  const organizations = useQuery(organizationStateQueryOptions(scope.organizationSlug)).data?.organizations ?? [];
  const own = organizations.filter((candidate) => candidate.access === "member");
  const supported = organizations.filter((candidate) => candidate.access === "support");

  function switchTo(slug: string) {
    // The open agent thread belongs to this Organization, so it stays behind.
    void navigate({ ...getDashboardDestination({ kind: "all", organizationSlug: slug }, section), search: { chat: undefined } });
  }

  return (
    <>
      <DropdownMenuRadioGroup value={scope.organizationSlug} onValueChange={switchTo}>
        {own.map((candidate) => (
          <DropdownMenuRadioItem key={candidate.id} value={candidate.slug}>
            {candidate.name}
          </DropdownMenuRadioItem>
        ))}
      </DropdownMenuRadioGroup>
      {supported.length > 0 ? (
        <>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuLabel>Support</DropdownMenuLabel>
            <DropdownMenuRadioGroup value={scope.organizationSlug} onValueChange={switchTo}>
              {supported.map((candidate) => (
                <DropdownMenuRadioItem key={candidate.id} value={candidate.slug}>
                  {candidate.name}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuGroup>
        </>
      ) : null}
    </>
  );
}
