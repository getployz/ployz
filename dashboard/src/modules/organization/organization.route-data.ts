import { notFound } from "@tanstack/react-router";
import type { RouteDataContext } from "#/collections/route-data";
import { organizationStateQueryOptions } from "./organization-state.queries";

export async function requireOrganization(context: RouteDataContext, organizationSlug: string) {
  const { activeOrganization, formerOrganization } = await context.queryClient.ensureQueryData(organizationStateQueryOptions(organizationSlug));
  if (activeOrganization?.slug !== organizationSlug) throw notFound(formerOrganization === null ? undefined : { data: { formerOrganization } });
  return activeOrganization;
}

/** The Organization Account opens in: the one the user is acting in, else their first. Undefined when they are in none. */
export async function requireAccountOrganizationSlug(context: RouteDataContext, activeOrganizationSlug: string | null | undefined) {
  if (activeOrganizationSlug) return activeOrganizationSlug;
  const { organizations } = await context.queryClient.ensureQueryData(organizationStateQueryOptions());
  return organizations.find((organization) => organization.access === "member")?.slug;
}

/** Billing exists only on Ployz-hosted Cloud. */
export async function requireBilling(context: RouteDataContext, organizationSlug: string) {
  const organization = await context.queryClient.ensureQueryData(organizationStateQueryOptions(organizationSlug));
  if (!organization.billingEnabled) throw notFound();
}
