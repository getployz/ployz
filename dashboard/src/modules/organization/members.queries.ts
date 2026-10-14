import { queryOptions } from "@tanstack/react-query";
import { listMembersServerFn, readInviteServerFn } from "#/modules/organization/members.functions";

export const membersKey = (organizationSlug: string) => ["organization-members", organizationSlug] as const;

/** The Organization's members and its pending invites. */
export function organizationMembersQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: membersKey(organizationSlug),
    queryFn: () => listMembersServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}

export type Invite = NonNullable<Awaited<ReturnType<typeof readInviteServerFn>>>;

export function inviteQueryOptions(token: string) {
  return queryOptions({
    queryKey: ["invite", token] as const,
    queryFn: () => readInviteServerFn({ data: { token } }),
    staleTime: 60_000,
  });
}
