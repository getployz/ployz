// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { clientWithRole, ROLE_STATES, type RoleState } from "#/test/your-role";
import { membersKey, useYouMay } from "./members.queries";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const EXPECTED = {
  owner: { "servers:operate": true, "org:delete": true, "environments:protect": true },
  admin: { "servers:operate": true, "org:delete": false, "environments:protect": true },
  member: { "servers:operate": false, "org:delete": false, "environments:protect": false },
  support: { "servers:operate": true, "org:delete": false, "environments:protect": false },
  loading: { "servers:operate": undefined, "org:delete": undefined, "environments:protect": undefined },
  failed: { "servers:operate": undefined, "org:delete": undefined, "environments:protect": undefined },
  "cached-failed": { "servers:operate": undefined, "org:delete": undefined, "environments:protect": undefined },
} satisfies Record<RoleState, Record<"servers:operate" | "org:delete" | "environments:protect", boolean | undefined>>;

it.each(ROLE_STATES)("knows what you may do only from a read of your role that succeeded (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  if (state === "cached-failed") expect(client.getQueryState(membersKey("ada"))).toMatchObject({ status: "error", data: { members: [{ role: "owner" }] } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => ({
    "servers:operate": useYouMay("ada", "servers:operate"),
    "org:delete": useYouMay("ada", "org:delete"),
    "environments:protect": useYouMay("ada", "environments:protect"),
  }), { wrapper });
  expect(result.current).toEqual(EXPECTED[state]);
});
