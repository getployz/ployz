import { queryOptions, useQuery, type UseQueryResult } from "@tanstack/react-query";
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
  return youMay(useQuery(organizationMembersQueryOptions(organizationSlug)), permission);
}

function youMay(members: UseQueryResult<Members>, permission: Permission) {
  const role = members.status !== "success" ? undefined : members.data.support ? "support" : members.data.members.find((row) => row.you)?.role;
  return role === undefined ? undefined : can(role, permission);
}

export const ROLE_CHECKING = "Checking your role…";
export const ROLE_UNCHECKED = "Couldn't check your role. Reload to try again.";
export const NO_LONGER_MEMBER = "You're no longer a member of this organization.";

/** A control gated on `useYouMay`, and why it is closed: `refusal` once your role says no, or that your role is still unknown. */
export interface RoleGate {
  allowed: boolean;
  reason: string | undefined;
}

export function useRoleGate(organizationSlug: string, permission: Permission, refusal?: string): RoleGate {
  const members = useQuery(organizationMembersQueryOptions(organizationSlug));
  const may = youMay(members, permission);
  if (may === true) return { allowed: true, reason: undefined };
  if (members.status === "pending") return { allowed: false, reason: ROLE_CHECKING };
  if (members.status === "error") return { allowed: false, reason: ROLE_UNCHECKED };
  return { allowed: false, reason: may === undefined ? NO_LONGER_MEMBER : refusal };
}

type Members = Awaited<ReturnType<typeof listMembersServerFn>>;

export type Invite = NonNullable<Awaited<ReturnType<typeof readInviteServerFn>>>;

export function inviteQueryOptions(token: string) {
  return queryOptions({
    queryKey: ["invite", token] as const,
    queryFn: () => readInviteServerFn({ data: { token } }),
    staleTime: 60_000,
  });
}
