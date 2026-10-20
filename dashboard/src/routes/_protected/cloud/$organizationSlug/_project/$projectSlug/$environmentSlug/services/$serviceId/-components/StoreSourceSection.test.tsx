// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { RowId, SettingRow } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as selectors from "#/components/service-source-selector";
import * as github from "#/modules/github/github.queries";
import * as gitFields from "./StoreGitFields";
import { StoreSourceSection, type StoreService } from "./StoreServiceDrawer";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("shows the picked image as the dialog closes, and the stored one again when the write is refused", async () => {
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(gitFields, "useRepositoryRef").mockReturnValue(undefined);
  vi.spyOn(gitFields, "useSourceLock").mockReturnValue(undefined);
  vi.spyOn(selectors, "ImageSelectorDialog").mockImplementation(({ open, initialImage, onSelectImage }) =>
    open ? <button type="button" data-initial={initialImage} onClick={() => onSelectImage("redis:7.2-alpine")}>Continue</button> : null);
  let fail = (_error: Error) => {};
  const write = new Promise<void>((_resolve, reject) => { fail = reject; });
  const state = asTestDouble<StoreService>()({
    organizationSlug: "ada",
    environment: { project: "shop", environment: "production" },
    service: { id: "redis-id", row: "redis-id:node" as RowId, name: "redis", private_dns: "redis", source: "image", change: null, template: null },
    rows: new Map([["image", asTestDouble<SettingRow>()({ value: "redis:7-alpine" })]]),
    changes: new Map(),
    source: "image" as const,
    path: (name: string) => `redis.${name}`,
    edit: vi.fn(),
    set: vi.fn(() => ({ isPersisted: { promise: write } })),
  });

  render(<StoreSourceSection state={state} />);
  fireEvent.click(screen.getByRole("button", { name: "Change container image" }));
  expect(screen.getByRole("button", { name: "Continue" }).getAttribute("data-initial")).toBe("redis:7-alpine");
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  expect(state.set).toHaveBeenCalledWith("image", "redis:7.2-alpine");
  expect(screen.queryByRole("button", { name: "Continue" })).toBeNull();
  expect(screen.getByText("redis:7.2-alpine")).toBeTruthy();
  expect(screen.queryByText("redis:7-alpine")).toBeNull();

  await act(async () => {
    fail(new Error("refused"));
    await write.catch(() => {});
  });
  expect(screen.getByText("redis:7-alpine")).toBeTruthy();
});

const LOCKED = "Only an owner or admin can change this.";

function gitService(source: "git" | "empty") {
  return asTestDouble<StoreService>()({
    organizationSlug: "ada",
    environment: { project: "shop", environment: "production" },
    service: { id: "web-id", row: "web-id:node" as RowId, name: "web", private_dns: "web", source, change: null, template: null },
    rows: new Map(source === "git" ? [
      ["repository", asTestDouble<SettingRow>()({ value: "ada/web" })],
      ["branch", asTestDouble<SettingRow>()({ value: "main" })],
    ] : []),
    changes: new Map(),
    source,
    path: (name: string) => `web.${name}`,
    edit: vi.fn(),
    set: vi.fn(),
  });
}

it("locks the repository and its branch in a Protected Environment, and says why", () => {
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(gitFields, "useRepositoryRef").mockReturnValue({ repositoryId: 1, installationId: 2 });
  vi.spyOn(gitFields, "useSourceLock").mockReturnValue(LOCKED);

  render(<StoreSourceSection state={gitService("git")} />);

  expect(screen.getByRole("button", { name: "Change repository" }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("button", { name: /Change branch/ }).hasAttribute("disabled")).toBe(true);
  expect(screen.getAllByText(LOCKED)).toHaveLength(2);
  fireEvent.click(screen.getByRole("button", { name: "Source actions" }));
  expect(screen.getByRole("menuitem", { name: "Switch to container image" }).getAttribute("aria-disabled")).toBe("true");
  expect(screen.getByRole("menuitem", { name: "Disconnect" }).getAttribute("aria-disabled")).toBe("true");
});

it("leaves the repository and branch open when the Environment isn't locked to you", () => {
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(gitFields, "useRepositoryRef").mockReturnValue({ repositoryId: 1, installationId: 2 });
  vi.spyOn(gitFields, "useSourceLock").mockReturnValue(undefined);

  render(<StoreSourceSection state={gitService("git")} />);

  expect(screen.getByRole("button", { name: "Change repository" }).hasAttribute("disabled")).toBe(false);
  expect(screen.getByRole("button", { name: /Change branch/ }).hasAttribute("disabled")).toBe(false);
  expect(screen.queryByText(LOCKED)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Source actions" }));
  expect(screen.getByRole("menuitem", { name: "Switch to container image" }).getAttribute("aria-disabled")).toBeNull();
  expect(screen.getByRole("menuitem", { name: "Disconnect" }).getAttribute("aria-disabled")).toBeNull();
});

it("offers only an image to an empty Service in a Protected Environment", () => {
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(gitFields, "useRepositoryRef").mockReturnValue(undefined);
  vi.spyOn(gitFields, "useSourceLock").mockReturnValue(LOCKED);

  render(<StoreSourceSection state={gitService("empty")} />);

  expect(screen.getByRole("button", { name: /GitHub repository/ }).hasAttribute("disabled")).toBe(true);
  expect(screen.getByRole("button", { name: /Container image/ }).hasAttribute("disabled")).toBe(false);
});

it("says once, under Auto-deploy, that pushes from a public repository need the GitHub App", () => {
  vi.spyOn(github, "useGithubWebOrigin").mockReturnValue("https://github.com");
  vi.spyOn(gitFields, "useRepositoryRef").mockReturnValue(undefined);
  vi.spyOn(gitFields, "useSourceLock").mockReturnValue(undefined);
  const note = "Pushes reach Ployz only once the GitHub App is installed on ada/web.";
  const policy = (value: boolean) => asTestDouble<SettingRow>()({ value, default: value, apply: "immediate", note });
  const state = gitService("git");
  state.rows.set("autoDeploy", policy(true));
  state.rows.set("waitForCi", policy(true));

  render(<StoreSourceSection state={state} />);

  expect(screen.getAllByText(note)).toHaveLength(1);
  expect(screen.getByText(/Hold an auto-deploy/)).toBeTruthy();
});
