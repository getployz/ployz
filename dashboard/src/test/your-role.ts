import { QueryClient } from "@tanstack/react-query";
import { waitFor } from "@testing-library/react";
import { expect, vi } from "vitest";
import * as memberFunctions from "#/modules/organization/members.functions";
import { membersKey, organizationMembersQueryOptions, ROLE_CHECKING, ROLE_UNCHECKED } from "#/modules/organization/members.queries";
import { can, type Permission } from "#/modules/organization/permissions";

/**
 * How the read of your role stands: a known role, a support visit, still loading, failed, or failed after an earlier
 * read cached an owner.
 */
export const ROLE_STATES = ["owner", "admin", "member", "support", "loading", "failed", "cached-failed"] as const;
export type RoleState = (typeof ROLE_STATES)[number];

type Members = Awaited<ReturnType<typeof memberFunctions.listMembersServerFn>>;
type Role = Members["members"][number]["role"];

const membersAs = (role: Role): Members => ({
  members: [{ you: true, role, name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }],
  invites: [],
  mail: false,
  support: false,
});

/** A support visit's read of the members: none of them is you. */
const supportVisit: Members = { members: [], invites: [], mail: false, support: true };

/** Caches a read of the members that names you as `role`. */
export function seedRole(client: QueryClient, organizationSlug: string, role: Role) {
  client.setQueryData(membersKey(organizationSlug), membersAs(role));
  return client;
}

/** A QueryClient whose read of your role in `organizationSlug` stands at `state`. */
export async function clientWithRole(organizationSlug: string, state: RoleState) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const list = vi.spyOn(memberFunctions, "listMembersServerFn");
  if (state === "loading") {
    list.mockReturnValue(new Promise(() => {}));
    return client;
  }
  if (state === "failed" || state === "cached-failed") {
    list.mockRejectedValue(new Error("offline"));
    if (state === "cached-failed") client.setQueryData(membersKey(organizationSlug), membersAs("owner"), { updatedAt: 0 });
    await client.prefetchQuery(organizationMembersQueryOptions(organizationSlug));
    return client;
  }
  if (state === "support") {
    client.setQueryData(membersKey(organizationSlug), supportVisit);
    return client;
  }
  return seedRole(client, organizationSlug, state);
}

/**
 * Why a control gated on `permission` is closed at `state`: `refusal` when a known role, support's included, lacks it,
 * that the role is still being checked or couldn't be, or nothing when it is open.
 */
export function gateReason(state: RoleState, permission: Permission, refusal: string | undefined) {
  if (state === "loading") return ROLE_CHECKING;
  if (state === "failed" || state === "cached-failed") return ROLE_UNCHECKED;
  return can(state, permission) ? undefined : refusal;
}

/**
 * The control opens only to a known role that holds `permission`, and is otherwise closed with its reason as its title.
 * A failed read is tried again on mount, so its reason settles once that try fails too.
 */
export async function expectGate(control: HTMLElement, state: RoleState, permission: Permission, refusal: string) {
  const known = state === "owner" || state === "admin" || state === "member" || state === "support";
  expect(control).toHaveProperty("disabled", !(known && can(state, permission)));
  await waitFor(() => expect(control.getAttribute("title")).toBe(gateReason(state, permission, refusal) ?? null));
}
