// @vitest-environment jsdom
import { afterEach, expect, test, vi } from "vitest";
import type { AuthSession } from "./auth";

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });

test("boots from SSR without waiting for the auth transport", async () => {
  const fetch = vi.fn(() => new Promise<Response>(() => {}));
  vi.stubGlobal("fetch", fetch);
  const { authClient, initializeAuthSession } = await import("./auth-client");
  const data = { session: { id: "session", userId: "user" }, user: { id: "user" } } as AuthSession;
  initializeAuthSession(data);
  expect(authClient.$store.atoms["session"]?.get()).toMatchObject({ data, isPending: false });
  expect(fetch).not.toHaveBeenCalled();
  initializeAuthSession(null);
  expect(authClient.$store.atoms["session"]?.get().data).toEqual(data);
});
