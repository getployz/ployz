// @vitest-environment jsdom
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act } from "@testing-library/react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, expect, it, vi } from "vitest";
import { LoginPanel } from "./LoginPanel";

const panel = <LoginPanel title="Keep this sandbox" action="Continue with GitHub" callbackURL="/claim/s3cret" />;

async function routed(isServer: boolean) {
  const router = createRouter({
    routeTree: createRootRoute({ component: () => panel }),
    history: createMemoryHistory({ initialEntries: ["/"] }),
    isServer,
  });
  await router.load();
  return <RouterProvider router={router} />;
}

afterEach(() => vi.unstubAllGlobals());

/** A rendered link puts the claim secret in the DOM, where a replay restarted on Back would snapshot it. */
it("keeps the callback URL out of the page", async () => {
  const html = renderToString(await routed(true));
  expect(html).toMatch(/Continue with GitHub/);
  expect(html).not.toMatch(/s3cret/);
});

it("goes to GitHub sign-in with the callback URL on click", async () => {
  const assign = vi.fn();
  vi.stubGlobal("location", { ...window.location, assign });
  const host = document.body.appendChild(document.createElement("div"));
  const page = await routed(false);
  await act(async () => createRoot(host).render(page));
  expect(host.innerHTML).not.toMatch(/s3cret/);
  await act(async () => host.querySelector("button")?.click());
  expect(assign).toHaveBeenCalledWith(`/auth/github?${new URLSearchParams({ callbackURL: "/claim/s3cret" })}`);
});
