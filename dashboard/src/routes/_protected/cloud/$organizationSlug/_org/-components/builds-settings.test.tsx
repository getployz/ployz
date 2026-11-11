// @vitest-environment jsdom
import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as views from "#/modules/config-store/store-view.queries";
import * as writes from "#/modules/config-store/store-write";
import * as github from "#/modules/github/github.queries";
import * as servers from "#/modules/machines/use-servers";
import { asTestDouble } from "#/lib/test-double";
import { GITHUB_BUILD_WORKFLOW } from "#/modules/github/github-build-workflow";
import { can } from "#/modules/organization/permissions";
import { clientWithRole, gateReason, ROLE_STATES } from "#/test/your-role";
import { BUILD_ORDER_REFUSED, BuildsSettings } from "./builds-settings";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it.each(ROLE_STATES)("lets only a role that may change the Organization's settings choose where builds run, as the Store does (%s)", async (state) => {
  const client = await clientWithRole("ada", state);
  vi.spyOn(views, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof views.useStoreView>>()({ ok: true, value: { build_order: "github-then-servers" } }));
  vi.spyOn(writes, "useStoreWriter").mockReturnValue(asTestDouble<ReturnType<typeof writes.useStoreWriter>>()({ commit: vi.fn() }));
  vi.spyOn(github, "useGithubBuildRepositories").mockReturnValue(asTestDouble<ReturnType<typeof github.useGithubBuildRepositories>>()({ data: [], error: null }));
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(servers, "useServerList").mockReturnValue(asTestDouble<ReturnType<typeof servers.useServerList>>()({ servers: [] }));
  render(<QueryClientProvider client={client}><BuildsSettings organizationSlug="ada" /></QueryClientProvider>);
  const known = state === "owner" || state === "admin" || state === "member" || state === "support";
  const open = known && can(state, "org:settings");
  expect(screen.getByRole("combobox", { name: "Where builds run" })).toHaveProperty("disabled", !open);
  const reason = gateReason(state, "org:settings", BUILD_ORDER_REFUSED);
  if (reason === undefined) expect(screen.queryByText(/Only an owner|your role/)).toBeNull();
  else expect(await screen.findByText(reason)).toBeTruthy();
});

it("offers the new workflow to a repository whose file isn't Ployz's, and waits for its commit once GitHub's editor opens", async () => {
  const client = await clientWithRole("ada", "owner");
  vi.spyOn(views, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof views.useStoreView>>()({ ok: true, value: { build_order: "github-then-servers" } }));
  vi.spyOn(writes, "useStoreWriter").mockReturnValue(asTestDouble<ReturnType<typeof writes.useStoreWriter>>()({ commit: vi.fn() }));
  const repositories = vi.spyOn(github, "useGithubBuildRepositories").mockReturnValue(asTestDouble<ReturnType<typeof github.useGithubBuildRepositories>>()({
    data: [{ installationId: 7, repositoryId: 42, fullName: "acme/blog", defaultBranch: "main", readiness: "outdated" }], error: null,
  }));
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(servers, "useServerList").mockReturnValue(asTestDouble<ReturnType<typeof servers.useServerList>>()({ servers: [] }));
  render(<QueryClientProvider client={client}><BuildsSettings organizationSlug="ada" /></QueryClientProvider>);
  expect(screen.getByText("Uses an older workflow")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Update workflow" }));
  expect(screen.getByLabelText("New workflow file").textContent).toBe(GITHUB_BUILD_WORKFLOW);
  const editor = (await screen.findByText("Open in GitHub ↗")).closest("a");
  if (editor === null) throw new Error("no link to GitHub's editor");
  expect(editor.getAttribute("href")).toBe("https://github.com/acme/blog/edit/main/.github/workflows/ployz-build.yml");
  fireEvent.click(editor);
  expect(await screen.findByText("Waiting for commit")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Update workflow" })).toBeNull();
  expect(repositories).toHaveBeenLastCalledWith("ada", new Set(["7:42"]));
});
