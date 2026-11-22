import { useLoaderData } from "@tanstack/react-router";

/** What the Organization's route loaded for every page under it. */
export function useOrganizationContext() {
  return useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
}
