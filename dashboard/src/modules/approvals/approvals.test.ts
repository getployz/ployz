import type { ConfigCommand, DestructiveEffect, DiffView, JsonValue, ServiceSettingChange } from "@ployz/sdk";
import { expect, it } from "vitest";
import { type ApprovalReview, approvalBadge, approvalCommand, approvalSubject, approvalTarget, asksApproval, destroyedLine, destroyedLines, withoutSealedBefore } from "#/modules/approvals/approvals";
import { asTestDouble } from "#/lib/test-double";

const here = { project: null, environment: null };
const id = "00000000-0000-4000-8000-0000000a5301";
const removeStaging: ConfigCommand = { command: "remove", project: "shop", environment: "staging" };
const shutDown: ConfigCommand = { command: "admit", admit: "shut_down", id, environment: here, version: null, accept_volume_loss: [] };
const cancel: ConfigCommand = { command: "cancel", deployment: id };
const retry: ConfigCommand = { command: "admit", admit: "retry", id, deployment: id, rolls_back: null, version: null, accept_volume_loss: [] };

it("asks for a removal or shutdown from `ployz` or an agent, never from the dashboard", () => {
  for (const via of ["agent", "cli"] as const) {
    expect(asksApproval(via, removeStaging)).toBe(true);
    expect(asksApproval(via, shutDown)).toBe(true);
  }
  expect(asksApproval("dashboard", removeStaging)).toBe(false);
  expect(asksApproval("dashboard", shutDown)).toBe(false);
});

it("sends a retry from `ployz` as one that may ask, since retrying a shutdown takes Services off", () => {
  expect(asksApproval("cli", retry)).toBe(true);
  expect(asksApproval("agent", retry)).toBe(true);
  expect(asksApproval("dashboard", retry)).toBe(false);
});

it("sends a cancel from `ployz` as one that may ask, and names it a Cancel", () => {
  expect(asksApproval("cli", cancel)).toBe(true);
  expect(asksApproval("agent", cancel)).toBe(true);
  expect(asksApproval("dashboard", cancel)).toBe(false);
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "production", project: "shop" } } });
  expect(approvalCommand(cancel)).toBe("cancel");
  expect(approvalSubject(approvalCommand(cancel), review)).toMatchObject({ verb: "Cancel", name: "production" });
  expect(approvalBadge(approvalCommand(cancel), review)).toBe("Cancels deploy");
});

it("names a shutdown as a shutdown, not a Deploy", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "staging", project: "shop" } } });
  expect(approvalCommand(shutDown)).toBe("shut_down");
  expect(approvalSubject(approvalCommand(shutDown), review)).toMatchObject({ verb: "Shut down", name: "staging" });
});

it("names a shutdown's retry a shutdown, so an approver never reads it as a redeploy", () => {
  const environment = { name: "production", project: "shop" };
  const removes = asTestDouble<ApprovalReview>()({ effects: [{ kind: "removes_service" }], diff: { environment } });
  expect(approvalSubject(approvalCommand(retry), removes)).toMatchObject({ verb: "Shut down", name: "production" });
  expect(approvalBadge(approvalCommand(retry), removes)).toBe("Destroys 1 thing");
  const redeploy = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment }, redeploy: { deployment: 4, rolls_back: [] } });
  expect(approvalSubject(approvalCommand(retry), redeploy)).toMatchObject({ verb: "Redeploy" });
  expect(approvalBadge(approvalCommand(retry), redeploy)).toBe("Redeploys");
});

it("lists a Store write's Environment under its Project, so two production approvals read apart", () => {
  const review = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment: { name: "production", project: "mcp-lab" } } });
  expect(approvalTarget("deploy", review)).toEqual({ verb: "Deploy", name: "mcp-lab/production" });
});

it("badges a Protected Volume Run by what it does to the Volume, never as a Deploy", () => {
  const run = (verb: string) => asTestDouble<ApprovalReview>()({ effects: [], operation: { verb, name: "pg-data", preview: {} }, protected: { environment: "production" } });
  expect(approvalBadge("move", run("move"))).toBe("Moves volume");
  expect(approvalBadge("restore", run("restore"))).toBe("Restores volume");
  expect(approvalBadge("release", run("release"))).toBe("Releases volume");
  expect(approvalBadge("resize", run("resize"))).toBe("Resizes volume");
  expect(approvalSubject("move", run("move"))).toMatchObject({ verb: "Move", name: "pg-data" });
});

const deletedVolume: DestructiveEffect = { kind: "deletes_volume", name: "pgdata", node: "v1", path: "volumes/v1" };

it("says server removal loses access while the disk keeps volume data", () => {
  const review: ApprovalReview = { effects: [deletedVolume], operation: { subject: "server:box", verb: "remove", name: "box", preview: {} } };
  const line = destroyedLine(deletedVolume, "remove", review);
  expect(`${line.verb} ${line.name}${line.after}`).toBe("Loses access to volume pgdata. Its data stays on the Server's disk until wiped. Wipe or reinstall the disk to erase it; sudo ployz-uninstall keeps the data.");
  expect(approvalBadge("remove", review)).toBe("Removes server");
});

it("keeps real volume deletion wording for project removal, namespace clean and deploy", () => {
  for (const [subject, command] of [["project:shop", "remove"], ["namespace:shop", "clean"], ["server:box", "clean"], ["server:box", "forget"], ["servers:acme", "forget"]] as const) {
    const review: ApprovalReview = { effects: [deletedVolume], operation: { subject, verb: command, name: "shop", preview: {} } };
    expect(destroyedLine(deletedVolume, command, review)).toEqual({ verb: "Deletes volume", name: "pgdata", after: ". Not recoverable." });
    expect(approvalBadge(command, review)).toBe("Deletes 1 volume");
  }
  expect(destroyedLine(deletedVolume)).toEqual({ verb: "Deletes volume", name: "pgdata", after: ". Not recoverable." });
  expect(destroyedLine(deletedVolume, "publish")).toEqual({ verb: "Next deploy deletes volume", name: "pgdata", after: "." });
});

it("lists a pruning Deploy's removal of other services, with the Servers it couldn't check, as the card does (#3511)", () => {
  const environment = { name: "production", project: "verify" };
  const prune = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment }, prunes: true, dropped: ["machine-3"] });
  expect(destroyedLines("admit", prune)).toEqual([
    "Also removes any other service running in this environment. Couldn't check every server: large packets to machine-3 are dropped on the mesh path",
  ]);
  const checked = asTestDouble<ApprovalReview>()({ effects: [{ kind: "removes_service", name: "worker" }], diff: { environment }, prunes: true });
  expect(destroyedLines("admit", checked)).toEqual(["Removes service worker", "Also removes any other service running in this environment"]);
  const plain = asTestDouble<ApprovalReview>()({ effects: [], diff: { environment } });
  expect(destroyedLines("admit", plain)).toEqual([]);
});

it("keeps no plain value of a variable the reviewed change seals, as the Store keeps none once that change saves", () => {
  const change = (path: string, before: JsonValue, after: JsonValue): ServiceSettingChange =>
    ({ path, kind: "update", before, after, canRestore: true, row: null });
  const node = { type: "service", id: "svc", name: "web", row: "svc", lifecycle: "update", comparison: "head", data: null, restarts: [] } as const;
  const diff = asTestDouble<DiffView>()({
    changes: [{ ...node, settings: [
      change("web.env.TOKEN", "fake-plain", { secret: true }),
      change("web.env.GONE", "fake-gone", { secret: false }),
      change("web.env.OPEN", "fake-open", "fake-open-2"),
      change("web.image", "nginx:1", "nginx:2"),
    ] }],
    draft_changes: [{ ...node, settings: [change("web.env.TOKEN", "fake-plain", { secret: true })] }],
  });
  const sealed = withoutSealedBefore(diff);
  expect(JSON.stringify(sealed)).not.toContain("fake-plain");
  expect(JSON.stringify(sealed)).not.toContain("fake-gone");
  expect(sealed.changes[0]?.settings.map((row) => row.before)).toEqual([{ secret: true }, { secret: true }, "fake-open", "nginx:1"]);
  expect(sealed.draft_changes?.[0]?.settings[0]?.before).toEqual({ secret: true });
});
