import { queryOptions, useQuery } from "@tanstack/react-query";
import { listMembersServerFn, readInviteServerFn } from "#/modules/organization/members.functions";
import { can, type Permission } from "#/modules/organization/permissions";

export const membersKey = (organizationSlug: string) => ["organization-members", organizationSlug] as const;

/** The Organization's members and its pending invites. */
export function organizationMembersQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: membersKey(organizationSlug),
    queryFn: () => listMembersServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}

/**
 * Whether your role, or support's on a support visit, holds `permission`, or undefined while it is unknown: until the
 * members load, and whenever the latest read of them failed, even with an older role cached. Cloud decides those, so an
 * action opens only on true and names who may act only on false.
 */
export function useYouMay(organizationSlug: string, permission: Permission): boolean | undefined {
  const members = useQuery(organizationMembersQueryOptions(organizationSlug));
  const role = members.status !== "success" ? undefined : members.data.support ? "support" : members.data.members.find((row) => row.you)?.role;
  return role === undefined ? undefined : can(role, permission);
}

export type Invite = NonNullable<Awaited<ReturnType<typeof readInviteServerFn>>>;

export function inviteQueryOptions(token: string) {
  return queryOptions({
    queryKey: ["invite", token] as const,
    queryFn: () => readInviteServerFn({ data: { token } }),
    staleTime: 60_000,
  });
}
