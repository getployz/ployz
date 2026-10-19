import { QueryClient } from "@tanstack/react-query";
import { expect, vi } from "vitest";
import * as memberFunctions from "#/modules/organization/members.functions";
import { membersKey, organizationMembersQueryOptions } from "#/modules/organization/members.queries";

/** How the read of your role stands: a known role, still loading, failed, or failed after an earlier read cached an owner. */
export const ROLE_STATES = ["owner", "admin", "member", "loading", "failed", "cached-failed"] as const;
export type RoleState = (typeof ROLE_STATES)[number];

type Members = Awaited<ReturnType<typeof memberFunctions.listMembersServerFn>>;
type Role = Members["members"][number]["role"];

const membersAs = (role: Role): Members => ({
  members: [{ you: true, role, name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }],
  invites: [],
  mail: false,
});

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
  return seedRole(client, organizationSlug, state);
}

/** An owner or admin may act; a member is refused with `refusal`; while the role is unknown the control stays closed and says nothing. */
export function expectGate(control: HTMLElement, state: RoleState, refusal: string) {
  const open = state === "owner" || state === "admin";
  expect(control).toHaveProperty("disabled", !open);
  expect(control.getAttribute("title")).toBe(state === "member" ? refusal : null);
}
