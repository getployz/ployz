// @vitest-environment jsdom
import type { ProjectsView } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createBrowserHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { STALL_MS } from "#/hooks/use-exclusive-mutation";
import type { StoreResult } from "#/modules/config-store/store.contract";
import * as storeFunctions from "#/modules/config-store/store.functions";
import { projectsQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import * as tokenFunctions from "#/modules/identity/organization-token.functions";
import { TokenItem, TokensSettings } from "./tokens-settings";

type Made = Awaited<ReturnType<typeof tokenFunctions.createTokenServerFn>>;

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

async function openNewToken() {
  vi.spyOn(tokenFunctions, "listTokensServerFn").mockResolvedValue([]);
  let resolve: (made: Made) => void = () => {};
  const create = vi.spyOn(tokenFunctions, "createTokenServerFn").mockReturnValue(new Promise((settle) => { resolve = settle; }));
  window.history.replaceState(null, "", "/members");
  window.history.pushState(null, "", "/tokens");
  const history = createBrowserHistory();
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <TokensSettings organizationSlug="ada" /> }),
    history,
  });
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const noProjects: StoreResult<{ view: "projects" } & ProjectsView> = { ok: true, value: { view: "projects", projects: [] } };
  vi.spyOn(storeFunctions, "readStoreViewServerFn").mockResolvedValue(noProjects);
  queryClient.setQueryData(storeViewOptions("ada", scope, projectsQuery()).queryKey, noProjects);
  await router.load();
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  fireEvent.click(await screen.findByRole("button", { name: "New token" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "ci" } });
  fireEvent.change(screen.getByLabelText("Access"), { target: { value: "read" } });
  return { create, history, made: (secret: string) => act(async () => resolve({ secret } as Made)) };
}

it("makes one token when Make token is double-clicked", async () => {
  const { create, made } = await openNewToken();
  const submit = screen.getByRole("button", { name: "Make token" });
  fireEvent.click(submit);
  fireEvent.click(submit);
  await made("pzt_once");

  expect(create).toHaveBeenCalledTimes(1);
  expect((await screen.findByLabelText<HTMLInputElement>("Token")).value).toBe("pzt_once");
});

it("holds the dialog through Back until the secret shows", async () => {
  const { history, made } = await openNewToken();
  const submit = screen.getByRole("button", { name: "Make token" });
  fireEvent.click(submit);
  await waitFor(() => expect(submit.hasAttribute("disabled")).toBe(true));
  await act(async () => {
    window.history.back();
    await new Promise((settle) => setTimeout(settle, 50));
  });
  await made("pzt_kept");

  expect(history.location.pathname).toBe("/tokens");
  expect((await screen.findByLabelText<HTMLInputElement>("Token")).value).toBe("pzt_kept");
});

it("lets go of a create that never answers, so Back and Retry work again", async () => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  try {
    const { create, history } = await openNewToken();
    const submit = screen.getByRole("button", { name: "Make token" });
    fireEvent.click(submit);
    await waitFor(() => expect(submit.hasAttribute("disabled")).toBe(true));
    await act(async () => vi.advanceTimersByTime(STALL_MS));

    expect(screen.getByText("Ployz hasn't answered. Retry or cancel.")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Cancel" }).hasAttribute("disabled")).toBe(false);
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
    await act(async () => vi.advanceTimersByTime(STALL_MS));
    await act(async () => {
      window.history.back();
      await vi.advanceTimersByTimeAsync(50);
    });
    expect(history.location.pathname).toBe("/members");
  } finally {
    vi.useRealTimers();
  }
});

const token = {
  name: "ci-deploy",
  scope: "Deploy staging",
  expires_at: "2027-01-01T00:00:00Z",
  expired: false,
};

it("asks before revoking a token, and revokes only on confirm", async () => {
  const onRevoke = vi.fn();
  render(<TokenItem token={token} revoking={false} onRevoke={onRevoke} />);
  fireEvent.click(screen.getByRole("button", { name: "Revoke" }));

  const dialog = await screen.findByRole("alertdialog", { name: "Revoke ci-deploy?" });
  expect(within(dialog).getByText("Anything using it stops working.")).toBeTruthy();
  expect(onRevoke).not.toHaveBeenCalled();

  fireEvent.click(within(dialog).getByRole("button", { name: "Revoke" }));
  expect(onRevoke).toHaveBeenCalledOnce();
});

it("keeps the token when the confirm is cancelled", async () => {
  const onRevoke = vi.fn();
  render(<TokenItem token={token} revoking={false} onRevoke={onRevoke} />);
  fireEvent.click(screen.getByRole("button", { name: "Revoke" }));

  fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
  expect(onRevoke).not.toHaveBeenCalled();
});
