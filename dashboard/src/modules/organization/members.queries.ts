/** @data-source remote: an Organization's members and pending invites, prefetched by Organization Settings and refetched after this tab's member writes; the change stream refetches it when a member or invite row changes. An invite link's Organization and state, prefetched by its page, which joining leaves */
import { queryOptions, useMutation, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import { optimisticCache } from "#/lib/optimistic";
import {
  leaveOrganizationServerFn,
  listMembersServerFn,
  readInviteServerFn,
  removeMemberServerFn,
  revokeInviteServerFn,
  setMemberRoleServerFn,
} from "#/modules/organization/members.functions";
import { can, type MemberRole, type Permission } from "#/modules/organization/permissions";
import { defineChange } from "#/collections/change-log/declare";

export const membersKey = (organizationSlug: string) => ["organization-members", organizationSlug] as const;

export const membersChange = defineChange({
  name: "members",
  sources: ["member", "invitation"],
  invalidate: (organizationSlug, scope) => void scope.queryClient.invalidateQueries({ queryKey: membersKey(organizationSlug) }),
});

/** The Organization's members and its pending invites. */
export function organizationMembersQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: membersKey(organizationSlug),
    queryFn: () => listMembersServerFn({ data: { organizationSlug } }),
    staleTime: 30_000,
  });
}

/** The member writes of Organization Settings. Each shows at once and rolls back with a toast when the save fails. */
export function useMemberWrites(organizationSlug: string) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const queryKey = membersKey(organizationSlug);
  const refresh = () => queryClient.invalidateQueries({ queryKey });
  const optimistic = <Input,>(fallback: string, apply: (current: Members, input: Input) => Members) => {
    const { onMutate, rollback } = optimisticCache(queryClient, queryKey, apply);
    return {
      onMutate,
      onError: (cause: unknown, _input: Input, context: Parameters<typeof rollback>[0]) => {
        rollback(context);
        toast.error(toErrorMessage(cause, fallback));
      },
      onSettled: refresh,
    };
  };
  const setRole = useMutation({
    mutationFn: (input: { member: string; role: MemberRole }) => setMemberRoleServerFn({ data: { organizationSlug, ...input } }),
    ...optimistic<{ member: string; role: MemberRole }>("Couldn't change the role.", (current, input) => ({
      ...current,
      members: current.members.map((row) => row.email === input.member ? { ...row, role: input.role } : row),
    })),
  });
  const remove = useMutation({
    mutationFn: (member: string) => removeMemberServerFn({ data: { organizationSlug, member } }),
    ...optimistic<string>("Couldn't remove the member.", (current, member) => ({
      ...current,
      members: current.members.filter((row) => row.email !== member),
    })),
  });
  const revokeInvite = useMutation({
    mutationFn: (invite: string) => revokeInviteServerFn({ data: { organizationSlug, invite } }),
    ...optimistic<string>("Couldn't revoke the invite.", (current, invite) => ({
      ...current,
      invites: current.invites.filter((row) => row.id !== invite),
    })),
  });
  const leave = useMutation({
    mutationFn: () => leaveOrganizationServerFn({ data: { organizationSlug } }),
    // The session acts in another Organization now, or none.
    onSuccess: () => navigate({ to: "/cloud", replace: true, reloadDocument: true }),
  });
  return { setRole, remove, revokeInvite, leave, refresh };
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

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

/**
 * A `useRoleGate` for what Ployz support never does, even where its role holds `permission`: a support visit, which
 * the Organization's loader names, closes the control as it closes for a role that lacks it.
 */
export function useMemberGate(organizationSlug: string, permission: Permission, refusal?: string): RoleGate {
  const { access } = organizationRoute.useLoaderData();
  const gate = useRoleGate(organizationSlug, permission, refusal);
  return access === "support" ? { allowed: false, reason: gate.reason } : gate;
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
