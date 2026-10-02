// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChangeGroup, ChangeRow } from "#/modules/config-store/store-deployments";
import { EnvironmentChangesReview, type EnvironmentChangesReviewProps } from "./EnvironmentChangesReview";

const nginx: ChangeGroup = {
  nodeType: "service",
  nodeId: "00000000-0000-4000-8000-000000000001",
  nodeName: "nginx",
  discardPath: "nginx",
  changeCount: 1,
  serviceSourceType: "image",
  lifecycle: "create",
  canDiscard: true,
  rows: [],
};

const row = (path: string, extra: Partial<ChangeRow> = {}): ChangeRow => ({
  changeKey: `api:${path}`, path, kind: "update", label: `Environment variable ${path.split(".").at(-1)}`,
  name: path.split(".").at(-1) ?? path, variable: true, currentValue: "60", newValue: "300", canDiscard: true, ...extra,
});
const api = (rows: ChangeRow[]): ChangeGroup => ({ ...nginx, nodeName: "api", discardPath: "api", lifecycle: "update", rows });
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
    const arrived = row("api.env.CACHE_TTL");
    review({
      groups: [api([own, arrived]), { ...nginx, nodeName: "web", discardPath: "web" }],
      originFor: (_, path) => path === arrived.path ? fromProduction : undefined,
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
      neverSyncFor: (_, path) => path === arrived.path ? neverSync : undefined,
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
