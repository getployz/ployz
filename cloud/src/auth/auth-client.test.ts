// @vitest-environment jsdom
import {afterEach, expect, test, vi } from "vitest";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

test.each([null, {
  session: { id: "session", userId: "user", activeOrganizationSlug: "nick" },
  user: { id: "user", name: "Nick", email: "nick@example.com" },
}])("initializes Better Auth before guards read its store: %j", async (data) => {
  let release = () => {};
  const pending = new Promise<void>((resolve) => { release = resolve; });
  const fetch = vi.fn(async () => {
    await pending;
    return Response.json(data);
  });
  vi.stubGlobal("fetch", fetch);
  const { authClient, initializeAuthSession } = await import("./auth-client");
  let ready = false;
  const initialization = initializeAuthSession().then(() => { ready = true; });
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
  expect(ready).toBe(false);
  release();
  await initialization;
  expect(authClient.$store.atoms["session"]?.get().data).toEqual(data);
  await initializeAuthSession();
  expect(fetch).toHaveBeenCalledTimes(1);
});

test("preserves a session lookup failure instead of treating it as signed out", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => Response.json({ message: "Unavailable" }, { status: 500 })));
  const { authClient, initializeAuthSession } = await import("./auth-client");
  await initializeAuthSession();
  expect(authClient.$store.atoms["session"]?.get().error?.status).toBe(500);
});
