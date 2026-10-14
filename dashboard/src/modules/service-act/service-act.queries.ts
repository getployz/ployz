import { queryOptions, useQuery } from "@tanstack/react-query";
import { latestServiceActServerFn } from "./service-act.functions";

/** Each Service's latest restart, start or stop; the change stream's `service_act` invalidates this root. */
export const serviceActKeys = { all: ["service-act"] as const };

type Subject = { readonly organizationSlug: string; readonly environmentId: string; readonly service: string };

/** How far a Service's latest restart, start or stop got. The change stream refetches it as each container moves. */
export function latestServiceActQueryOptions(subject: Subject) {
  return queryOptions({
    queryKey: [...serviceActKeys.all, subject.organizationSlug, subject.environmentId, subject.service] as const,
    queryFn: () => latestServiceActServerFn({ data: subject }),
    staleTime: 60_000,
  });
}

/** Undefined until the first read lands, and for no Service (`service` empty): the header shows no run rather than wait on one. */
export function useLatestServiceAct(subject: Subject) {
  return useQuery({ ...latestServiceActQueryOptions(subject), enabled: subject.service !== "" }).data;
}
