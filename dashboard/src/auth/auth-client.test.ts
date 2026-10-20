// @vitest-environment jsdom
import { afterEach, expect, onTestFinished, test, vi } from "vitest";
import { cleanStores } from "nanostores";
import type { AuthSession } from "./auth";
import { failOnMountedStore } from "#/test/window-teardown";

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules(); });
let session: typeof import("./auth-client").authClient.$store.atoms["session"] | undefined;
failOnMountedStore("Better Auth session", () => session);

test("boots from SSR without waiting for the auth transport", async () => {
  vi.resetModules();
  const fetch = vi.fn(() => new Promise<Response>(() => {}));
  vi.stubGlobal("fetch", fetch);
  const { authClient, initializeAuthSession } = await import("./auth-client");
  // isolate: false shares Better Auth's session store across files; start from its boot state.
  session = authClient.$store.atoms["session"];
  onTestFinished(() => cleanStores(session));
  session?.set({ ...session.get(), data: null, isPending: true });
  const data = { session: { id: "session", userId: "user" }, user: { id: "user" } } as AuthSession;
  initializeAuthSession(data);
  expect(authClient.$store.atoms["session"]?.get()).toMatchObject({ data, isPending: false });
  expect(fetch).not.toHaveBeenCalled();
  initializeAuthSession(null);
  expect(authClient.$store.atoms["session"]?.get().data).toEqual(data);
});
