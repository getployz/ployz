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
  vi.spyOn(selectors, "ImageSelectorDialog").mockImplementation(({ open, onSelectImage }) =>
    open ? <button type="button" onClick={() => onSelectImage("redis:7.2-alpine")}>Continue</button> : null);
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
