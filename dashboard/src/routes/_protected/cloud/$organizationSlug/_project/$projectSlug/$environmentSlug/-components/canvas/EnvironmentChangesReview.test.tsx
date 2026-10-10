// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RowId } from "@ployz/sdk";
import type { ChangeGroup, ChangeRow } from "#/modules/config-store/store-deployments";
import { EnvironmentChangesReview, type EnvironmentChangesReviewProps } from "./EnvironmentChangesReview";

const nginx: ChangeGroup = {
  nodeType: "service",
  nodeId: "00000000-0000-4000-8000-000000000001",
  nodeName: "nginx",
  discardPath: "nginx",
  changeCount: 1,
  restarts: [],
  serviceSourceType: "image",
  lifecycle: "create",
  canDiscard: true,
  row: "nginx:node" as RowId,
  rows: [],
};

const row = (path: string, extra: Partial<ChangeRow> = {}): ChangeRow => ({
  changeKey: `api:${path}`, path, kind: "update", label: `Environment variable ${path.split(".").at(-1)}`,
  name: path.split(".").at(-1) ?? path, row: null, variable: true, currentValue: "60", newValue: "300", canDiscard: true, ...extra,
});
const api = (rows: ChangeRow[]): ChangeGroup => ({ ...nginx, nodeName: "api", discardPath: "api", lifecycle: "update", rows });
const file = (path: string, configFile: NonNullable<ChangeRow["configFile"]>, extra: Partial<ChangeRow> = {}) => row(`configs.sentry.files.${path}`, {
  label: path, name: path, variable: false, currentValue: "File", newValue: "File", configFile, ...extra,
});
const config = (rows: ChangeRow[]): ChangeGroup => ({
  ...nginx, nodeType: "config", nodeId: "sentry", nodeName: "sentry", discardPath: "configs.sentry", lifecycle: "update", restarts: ["web", "worker"], rows,
});
const fromProduction = { title: "From production's deploy", description: "fix-api is a Branch of production, so what production deploys arrives here too." };

function review(props: Partial<EnvironmentChangesReviewProps> & Pick<EnvironmentChangesReviewProps, "groups">) {
  const handlers = { onClose: vi.fn(), onDiscardNode: vi.fn(), onDiscardRow: vi.fn(), onPublish: vi.fn(), onDiscardAll: vi.fn() };
  render(
    <EnvironmentChangesReview environment="fix-api" totalChanges={props.groups.flatMap((group) => group.rows).length || 1}
      canDeploy={false} canPublish onDeploy={vi.fn()} message="" onMessageChange={vi.fn()} {...handlers} {...props} />,
  );
  return handlers;
}

const lineOf = (text: string) => within(screen.getByText(text).closest("li") as HTMLElement);
async function menuOf(label: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${label}` }));
  return within(await screen.findByRole("menu"));
}

afterEach(cleanup);

describe("Details", () => {
  it("shows complete file text and metadata, distinguishing an empty file from a missing one", () => {
    const before = { content: "first line\nlast line\n", mode: "0444", uid: 0, gid: 0 };
    const after = { content: "new first line\nnew last line\n", mode: "0555", uid: 1000, gid: 1001 };
    review({ groups: [config([
      file("nested/app.conf", { before, after }),
      file("empty.conf", { before: null, after: { ...before, content: "" } }, { kind: "add" }),
      file("old.conf", { before, after: null }, { kind: "remove" }),
    ])] });
    const changed = lineOf("nested/app.conf");
    expect(changed.getByText("Before")).toBeTruthy();
    expect(changed.getByText("After")).toBeTruthy();
    for (const text of [before.content, after.content]) {
      const panel = changed.getByText(text, { exact: true, normalizer: (value) => value });
      expect(panel.tagName).toBe("PRE");
      expect(panel.closest(".ph-no-capture")).toBeTruthy();
      expect(panel.className).toContain("max-h-48");
    }
    for (const text of ["Mode 0555", "UID 1000", "GID 1001"]) {
      const metadata = changed.getByText(text);
      expect(metadata.className).toContain("text-changed-deep");
      expect(metadata.closest(".ph-no-capture")).toBeTruthy();
    }
    const added = lineOf("empty.conf");
    expect(added.queryByText("Before")).toBeNull();
    expect(added.getByText("After")).toBeTruthy();
    expect(added.getByText("Empty file")).toBeTruthy();
    const removed = lineOf("old.conf");
    expect(removed.getByText("Before")).toBeTruthy();
    expect(removed.queryByText("After")).toBeNull();
    expect(screen.queryByText(/\{"content":/)).toBeNull();
  });

  it("shows metadata-only edits and restart effects once across origins without adding a change", () => {
    const before = { content: "unchanged", mode: "0444", uid: 0, gid: 0 };
    const arrived = file("arrived.conf", { before, after: before }, { row: "s:files.arrived.conf" as RowId });
    const own = file("app.conf", { before, after: { ...before, mode: "0555" } });
    review({ groups: [config([arrived, own])], originFor: (at) => at === arrived.row ? fromProduction : undefined });
    const effects = screen.getAllByText("Deploying sentry restarts web and worker.");
    expect(effects).toHaveLength(1);
    expect(effects[0]?.closest("section")?.getAttribute("aria-label")).toBe("Your changes");
    expect(screen.getByText("2 changes in fix-api, not yet published.")).toBeTruthy();
    expect(lineOf("app.conf").getAllByText("unchanged")).toHaveLength(2);
    expect(lineOf("app.conf").getByText("Mode 0555").className).toContain("text-changed-deep");
  });

  it("keeps file Discard and Discard all of the Config reachable with the exact file path", async () => {
    const value = { content: "test", mode: "0444", uid: 0, gid: 0 };
    const group = config([file("nested/app.conf", { before: value, after: value }), file("app.conf.bak", { before: null, after: value })]);
    const test = review({ groups: [group] });
    const menu = await menuOf("sentry nested/app.conf");
    expect(menu.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Discard", "Discard all of sentry"]);
    fireEvent.click(menu.getByRole("menuitem", { name: "Discard" }));
    expect(test.onDiscardRow).toHaveBeenCalledWith(group, "configs.sentry.files.nested/app.conf");
    fireEvent.click((await menuOf("sentry nested/app.conf")).getByRole("menuitem", { name: "Discard all of sentry" }));
    expect(test.onDiscardNode).toHaveBeenCalledWith(group);
  });

  it("offers only whole Config discard for a file the Store cannot restore alone", async () => {
    const value = { content: "test", mode: "0444", uid: 0, gid: 0 };
    const group = config([file("app.conf", { before: null, after: value }, { canDiscard: false })]);
    const test = review({ groups: [group] });
    const menu = await menuOf("sentry app.conf");
    expect(menu.queryByRole("menuitem", { name: "Discard" })).toBeNull();
    fireEvent.click(menu.getByRole("menuitem", { name: "Discard all of sentry" }));
    expect(test.onDiscardNode).toHaveBeenCalledWith(group);
    expect(test.onDiscardRow).not.toHaveBeenCalled();
  });

  it("reads a new node as one line, and Discard in its menu puts it back", async () => {
    const test = review({ groups: [nginx] });

    expect(screen.getByText("1 change in fix-api, not yet published.")).toBeTruthy();
    expect(screen.getByText("nginx · will be added").className).toContain("text-success");
    // One Environment's own changes alone need no heading.
    expect(screen.queryByRole("heading", { level: 3 })).toBeNull();
    fireEvent.click((await menuOf("nginx")).getByRole("menuitem", { name: "Discard" }));
    expect(test.onDiscardNode).toHaveBeenCalledWith(nginx);
    expect(test.onClose).toHaveBeenCalledOnce();
  });

  it("groups what arrived apart from the Environment's own changes, said once in words", () => {
    const own = row("api.env.LOG_LEVEL", { currentValue: "warn", newValue: "debug" });
    const arrived = row("api.env.CACHE_TTL", { row: "a:variables.CACHE_TTL" as RowId });
    review({
      groups: [api([own, arrived]), { ...nginx, nodeName: "web", discardPath: "web" }],
      originFor: (at) => at === arrived.row ? fromProduction : undefined,
    });

    const [mine, theirs] = screen.getAllByRole("region");
    expect(within(mine as HTMLElement).getByRole("heading").textContent).toBe("Your changes");
    expect(within(mine as HTMLElement).getByText("LOG_LEVEL")).toBeTruthy();
    expect(within(mine as HTMLElement).getByText("web · will be added")).toBeTruthy();
    const incoming = within(theirs as HTMLElement);
    expect(incoming.getByRole("heading").textContent).toBe("From production's deploy");
    expect(incoming.getByText(fromProduction.description)).toBeTruthy();
    expect(incoming.getByText("CACHE_TTL").className).toContain("font-mono");
    expect(incoming.queryByText(/^From production$/u)).toBeNull();
    // The node in muted text, and old → new with the new value in the changed colour.
    const line = lineOf("CACHE_TTL");
    expect(line.getByText("api").className).toContain("text-muted-foreground");
    expect(line.getByText("300").className).toContain("text-changed-deep");
  });

  it("strikes through a removed value and colours an added one", () => {
    review({ groups: [api([
      row("api.env.LEGACY_AUTH", { kind: "remove", currentValue: "true", newValue: "" }),
      row("api.env.FEATURE_SEARCH", { kind: "add", currentValue: "", newValue: "on" }),
    ])] });

    expect(lineOf("LEGACY_AUTH").getByText("true").className).toContain("line-through");
    expect(lineOf("FEATURE_SEARCH").getByText("on").className).toContain("text-success");
  });

  it("offers Never sync beside Discard in the menu of a change that arrived, and closes as the last change goes", async () => {
    const neverSync = vi.fn();
    const arrived = row("api.env.CACHE_TTL");
    const test = review({
      groups: [api([arrived, row("api.replicas", { label: "Replicas", name: "Replicas", variable: false })])], totalChanges: 1,
      neverSyncFor: (line) => line === arrived ? neverSync : undefined,
    });

    const own = await menuOf("api Replicas");
    expect(own.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Discard"]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    const items = await menuOf("api Environment variable CACHE_TTL");
    expect(items.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Discard", "Never sync"]);
    fireEvent.click(items.getByRole("menuitem", { name: "Never sync" }));
    expect(neverSync).toHaveBeenCalledOnce();
    expect(test.onClose).toHaveBeenCalledOnce();
  });

  it("puts a change's note on a second line, and keeps Discard all and Publish in the footer", () => {
    const test = review({ groups: [api([row("api.env.LOG_LEVEL")])], noteFor: () => "production has since set info" });

    expect(lineOf("LOG_LEVEL").getByText("production has since set info")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Discard all" }));
    fireEvent.click(screen.getByRole("button", { name: "Publish" }));
    expect(test.onDiscardAll).toHaveBeenCalledOnce();
    expect(test.onPublish).toHaveBeenCalledOnce();
  });
});
