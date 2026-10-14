import { queryOptions, useQuery } from "@tanstack/react-query";
import { latestServiceActServerFn } from "./service-act.functions";

/** The change stream's `service_act` invalidates this root. */
export const serviceActKeys = { all: ["service-act"] as const };

type Subject = { readonly organizationSlug: string; readonly environmentId: string; readonly service: string };

export function latestServiceActQueryOptions(subject: Subject) {
  return queryOptions({
    queryKey: [...serviceActKeys.all, subject.organizationSlug, subject.environmentId, subject.service] as const,
    queryFn: () => latestServiceActServerFn({ data: subject }),
    staleTime: 60_000,
  });
}

export function useLatestServiceAct(subject: Subject) {
  return useQuery({ ...latestServiceActQueryOptions(subject), enabled: subject.service !== "" }).data;
}
