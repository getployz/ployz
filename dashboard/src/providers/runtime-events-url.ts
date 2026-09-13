import { linkOptions } from "@tanstack/react-router";

export function buildRuntimeEventsUrl(organizationSlug: string) {
  const link = linkOptions({
    to: "/api/runtime/events",
    search: { organizationSlug },
  });
  const search = new URLSearchParams(link.search);
  return `${link.to}?${search.toString()}`;
}
