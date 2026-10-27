// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DiffView } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/approvals/approvals.functions";
import { ApprovalCard } from "./approval-card";
import type { ApprovalView } from "./approvals.queries";

const decide = vi.spyOn(functions, "decideApprovalServerFn");
afterEach(() => { cleanup(); decide.mockReset(); });

// A Deploy that drops the pg-data Volume and also scales web.
const approval = asTestDouble<ApprovalView>()({
  id: "a1",
  status: "pending",
  digest: "sha256:plan",
  command: "admit",
  reason: null,
  created_at: "2026-10-08T12:00:00.000Z",
  decided_at: null,
  review: {
    effects: [{ kind: "deletes_volume" as const, name: "pg-data", node: "v1", path: "volumes.pg-data" }],
    diff: asTestDouble<DiffView>()({
      environment: { id: "e1", project: "shop", name: "production", revision: 4 },
      total_count: 2,
      changes: [
        { type: "volume", id: "v1", name: "pg-data", lifecycle: "delete", comparison: null, data: null, settings: [] },
        { type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
          { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
        ] },
      ],
    }),
  },
});

function show(seed: ApprovalView, onSettled = vi.fn()) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  render(<QueryClientProvider client={client}><ApprovalCard organizationSlug="acme" id={seed.id} seed={seed} onSettled={onSettled} /></QueryClientProvider>);
  return { onSettled, card: () => screen.getByLabelText("Deploy shop/production needs approval") };
}

/** A change line by its whole text: its name and values sit in their own spans. */
const line = (pattern: RegExp) => (_: string, element: Element | null) => element?.tagName === "LI" && pattern.test(element.textContent ?? "");

const answered = (status: ApprovalView["status"], reason: string | null = null) =>
  ({ ok: true as const, approval: { ...approval, status, reason, decided_at: "2026-10-08T12:01:00.000Z" } }) as never;

it("leads with what the plan destroys and folds the rest to a count", () => {
  const card = show(approval).card();
  expect(within(card).getByText("Deletes 1 volume")).toBeTruthy();
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Deletes volume pg-data. Not recoverable.");
  expect(within(card).queryByText(/1 → 2/)).toBeNull();
  fireEvent.click(within(card).getByText("1 other change"));
  expect(within(card).getByText(line(/web .*1 → 2/))).toBeTruthy();
});

const restoreOf = (at: number) => ({ ...approval, review: { ...approval.review, effects: [{
  kind: "detaches_volume" as const, name: "pg-data", node: "s1", path: "postgres.mounts.pg-data",
  lost_after: { at, restored: "pg-data-0927" },
}] } });

function lossShownAt(at: number, now: number) {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(now);
  try {
    const destroys = within(show(restoreOf(at)).card()).getByRole("list", { name: "Destroys" });
    expect(within(destroys).getByText("from postgres", { exact: false })).toBeTruthy();
    return within(destroys).getByText(/^Changes after /).textContent;
  } finally {
    vi.useRealTimers();
  }
}

it("says which changes a restore's detach leaves behind, in the approver's time", () => {
  const at = Date.UTC(2026, 9, 8, 9, 27) / 1000;
  const time = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit" }).format(new Date(at * 1000));
  expect(lossShownAt(at, at * 1000 + 60_000)).toBe(`Changes after ${time} are not in pg-data-0927`);
});

it("dates a restore point from another day, so an older point reads apart from today's", () => {
  const at = Date.UTC(2026, 9, 8, 9, 27) / 1000;
  const when = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }).format(new Date(at * 1000));
  expect(lossShownAt(at, at * 1000 + 3 * 86_400_000)).toBe(`Changes after ${when} are not in pg-data-0927`);
});

it("dates a restore point from today once midnight passes while the card is open", () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(new Date(2026, 9, 11, 23, 59));
  try {
    const destroys = within(show(restoreOf(new Date(2026, 9, 11, 23, 55).getTime() / 1000)).card()).getByRole("list", { name: "Destroys" });
    expect(within(destroys).getByText(/^Changes after /).textContent).toBe("Changes after 23:55 are not in pg-data-0927");
    act(() => vi.advanceTimersByTime(2 * 60_000));
    expect(within(destroys).getByText(/^Changes after /).textContent).toBe("Changes after 11 Oct, 23:55 are not in pg-data-0927");
  } finally {
    vi.useRealTimers();
  }
});

it("names the Project, so two Deploys to production read apart", () => {
  const blog = { ...approval, id: "a2", review: {
    effects: [],
    diff: asTestDouble<DiffView>()({ environment: { id: "e2", project: "blog", name: "production", revision: 1 }, total_count: 0, changes: [] }),
  } };
  show(approval);
  show(blog);
  const titles = [...document.querySelectorAll("[data-slot=card-title]")].map((title) => title.textContent);
  expect(titles).toEqual(["Deploy shop/production", "Deploy blog/production"]);
});

it("approves exactly the digest shown, then settles", async () => {
  decide.mockResolvedValue(answered("approved"));
  const { onSettled } = show(approval);
  fireEvent.click(screen.getByRole("button", { name: "Approve" }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "a1", decision: { approve: { digest: "sha256:plan" } } } });
  expect(screen.getByText(/Approved/)).toBeTruthy();
});

it("denies with the reason typed, for the agent to read", async () => {
  decide.mockResolvedValue(answered("denied", "keep the data"));
  const { onSettled } = show(approval);
  fireEvent.click(screen.getByRole("button", { name: "Deny with a reason…" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Reason" }), { target: { value: " keep the data " } });
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(decide).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "a1", decision: { reject: { reason: "keep the data" } } } });
  expect(screen.getByText("Denied: keep the data")).toBeTruthy();
});

it("shows Cloud's refusal and keeps the card when the answer doesn't land", async () => {
  decide.mockResolvedValue({ ok: false, message: "The plan changed since this was asked; run the command again to review it." } as never);
  const { onSettled } = show(approval);
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  expect((await screen.findByRole("alert")).textContent).toBe("The plan changed since this was asked; run the command again to review it.");
  expect(onSettled).not.toHaveBeenCalled();
});

it("decides only by its buttons: Esc and ⌘↵ neither approve nor deny", () => {
  const { card, onSettled } = show(approval);
  const approve = screen.getByRole("button", { name: "Approve" });
  fireEvent.keyDown(card(), { key: "Escape" });
  fireEvent.keyDown(approve, { key: "Escape" });
  fireEvent.keyDown(card(), { key: "Enter", metaKey: true });
  fireEvent.keyDown(approve, { key: "Enter", ctrlKey: true });
  fireEvent.click(screen.getByRole("button", { name: "Deny with a reason…" }));
  const reason = screen.getByRole("textbox", { name: "Reason" });
  fireEvent.change(reason, { target: { value: "keep the data" } });
  fireEvent.keyDown(reason, { key: "Escape" });
  fireEvent.keyDown(reason, { key: "Enter", metaKey: true });
  expect(decide).not.toHaveBeenCalled();
  expect(onSettled).not.toHaveBeenCalled();
  expect(screen.getByRole("button", { name: "Deny" })).toBeTruthy();
});

it("names a Server operation by its verb and Server, with nothing folded", () => {
  const removal = asTestDouble<ApprovalView>()({
    ...approval,
    command: "remove",
    review: {
      effects: [{ kind: "removes_server" as const, name: "fra-1", node: "m1", path: "servers/m1" }],
      operation: { verb: "remove" as const, name: "fra-1", preview: { machine: "m1" } },
    },
  });
  show(removal);
  const card = within(screen.getByLabelText("Remove fra-1 needs approval"));
  expect(card.getByText("Remove fra-1")).toBeTruthy();
  expect(card.getByRole("list", { name: "Destroys" }).textContent).toBe("Removes server fra-1");
  expect(card.queryByText(/other change/)).toBeNull();
});

it("says a Server removal leaves lost Volume data on the disk", () => {
  show(asTestDouble<ApprovalView>()({
    ...approval,
    command: "remove",
    review: {
      effects: [
        { kind: "removes_server", name: "fra-1", node: "m1", path: "servers/m1" },
        { kind: "deletes_volume", name: "pgdata", node: "v1", path: "volumes/v1" },
      ],
      operation: { subject: "server:m1", verb: "remove", name: "fra-1", preview: {} },
    },
  }));
  const card = within(screen.getByLabelText("Remove fra-1 needs approval"));
  expect(card.getByText("Removes server", { selector: '[data-slot="badge"]' })).toBeTruthy();
  expect(card.getByRole("list", { name: "Effects" }).textContent).toContain("Loses access to volume pgdata. Its data stays on the Server's disk until wiped.");
  expect(card.getByRole("list", { name: "Effects" }).textContent).toContain("Wipe or reinstall the disk to erase it; sudo ployz-uninstall keeps the data.");
  expect(card.queryByText(/Not recoverable/)).toBeNull();
});

it("says a Save deletes nothing until the next deploy, and approving it saves without deploying", async () => {
  decide.mockResolvedValue(({ ok: true, approval: { ...approval, command: "publish", status: "approved", decided_at: "2026-10-08T12:01:00.000Z" } }) as never);
  const { onSettled } = show(asTestDouble<ApprovalView>()({ ...approval, command: "publish" }));
  const card = within(screen.getByLabelText("Save shop/production needs approval"));
  expect(card.getByText("Save shop/production")).toBeTruthy();
  expect(card.getByText("Deletes on deploy")).toBeTruthy();
  expect(card.getByRole("list", { name: "Destroys" }).textContent).toBe("Next deploy deletes volume pg-data.");
  fireEvent.click(card.getByRole("button", { name: /^Approve/ }));
  await waitFor(() => expect(onSettled).toHaveBeenCalled());
  expect(screen.getByText("Approved. Saves without deploying.")).toBeTruthy();
});

it("counts a Deploy's deleted Volumes, and anything else it destroys as things", () => {
  const two = { ...approval.review, effects: [approval.review.effects[0], { kind: "deletes_volume" as const, name: "cache", node: "v2", path: "volumes.cache" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review: two }));
  expect(within(screen.getByLabelText("Deploy shop/production needs approval")).getByText("Deletes 2 volumes")).toBeTruthy();
  cleanup();
  const mixed = { ...approval.review, effects: [approval.review.effects[0], { kind: "removes_service" as const, name: "web", node: "s1", path: "services.s1" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review: mixed }));
  expect(within(screen.getByLabelText("Deploy shop/production needs approval")).getByText("Destroys 2 things")).toBeTruthy();
});

it("warns that an agent's rollback deletes Volume data, beside what it undoes", () => {
  const rollback = { ...approval, id: "a3", command: "retry", review: {
    effects: [{ kind: "deletes_volume" as const, name: "data", node: "v5", path: "volumes.data" }],
    diff: asTestDouble<DiffView>()({ environment: { id: "e1", project: "shop", name: "production", revision: 4 }, total_count: 0, changes: [] }),
    redeploy: { deployment: 1, rolls_back: [2] },
  } };
  show(rollback);
  expect(screen.getByText("Back to #1. Undoes #2.")).toBeTruthy();
  expect(screen.getByRole("list", { name: "Destroys" }).textContent).toBe("Deletes volume data. Not recoverable.");
});

it("names what a redeploy that replays a teardown removes, beside the Deployment it ships again", () => {
  const replay = { ...approval, id: "a4", command: "retry", protected: true, yours: false, review: {
    effects: [
      { kind: "removes_service" as const, name: "extra", node: "s3", path: "extra" },
      { kind: "removes_domain" as const, name: "app.example.com", node: "s1", path: "web.routes.r1" },
    ],
    diff: asTestDouble<DiffView>()({ environment: { id: "e1", project: "shop", name: "production", revision: 4 }, total_count: 0, changes: [] }),
    redeploy: { deployment: 14, rolls_back: [] },
  } };
  show(replay);
  expect(screen.getByText("Ships #14 again.")).toBeTruthy();
  expect(screen.getByText("Redeploys")).toBeTruthy();
  const destroys = within(screen.getByRole("list", { name: "Destroys" })).getAllByRole("listitem").map((item) => item.textContent);
  expect(destroys).toEqual([expect.stringMatching(/extra/), expect.stringMatching(/app\.example\.com/)]);
});

it("words a destroyed thing without a name as its kind, never by its id", () => {
  const review = { ...approval.review, effects: [{ kind: "removes_service" as const, node: "0b9c5d1e-svc", path: "services.0b9c5d1e-svc" }] };
  show(asTestDouble<ApprovalView>()({ ...approval, review }));
  expect(within(screen.getByLabelText("Deploy shop/production needs approval")).getByRole("list", { name: "Destroys" }).textContent).toBe("Removes a service");
});

it("says the plan changed when the approval was superseded, and settles", () => {
  const { onSettled } = show({ ...approval, status: "superseded" });
  expect(screen.getByText("The plan changed since this was asked. Ask again to review the new one.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Approve/ })).toBeNull();
  expect(onSettled).toHaveBeenCalled();
});

it("says who asked, and shows the change their ask saves apart from what else it deploys", () => {
  const diff = asTestDouble<DiffView>()({
    ...approval.review.diff,
    changes: [
      { type: "service", id: "s2", name: "api", lifecycle: "create", comparison: "introduction", data: null, settings: [] },
      { type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
      ] },
    ],
    draft_changes: [
      { type: "service", id: "s2", name: "api", lifecycle: "update", comparison: "saved", data: null, settings: [
        { path: "api.env.LINUS_CHANGE", kind: "add", before: null, after: "requested", canRestore: true, row: null },
      ] },
    ],
  });
  show(asTestDouble<ApprovalView>()({ ...approval, protected: true, yours: false, asker: "Linus Torvalds", review: { effects: [], diff } }));
  const card = within(screen.getByLabelText("Deploy shop/production needs approval"));
  expect(card.getByText(/^Asked by Linus Torvalds ·/)).toBeTruthy();
  expect(card.getByRole("list", { name: "Saves" }).textContent).toBe("+ api LINUS_CHANGE requested");
  expect(card.getByText("Saves")).toBeTruthy();
  fireEvent.click(card.getByText("2 other changes"));
  expect(card.getByText(line(/api new service/))).toBeTruthy();
  expect(card.getByText(line(/web .*1 → 2/))).toBeTruthy();
});

it("lists a Sync's changes without claiming it saves the target's unsaved edit", () => {
  const diff = asTestDouble<DiffView>()({
    ...approval.review.diff,
    changes: [
      { type: "service", id: "s1", name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
        { path: "web.replicas", kind: "update", before: 1, after: 2, canRestore: true, row: null },
      ] },
    ],
    draft_changes: [
      { type: "service", id: "s3", name: "redis", lifecycle: "create", comparison: "saved", data: null, settings: [] },
    ],
  });
  show(asTestDouble<ApprovalView>()({ ...approval, command: "sync", protected: true, yours: false, asker: "Linus Torvalds", review: { effects: [], diff } }));
  const card = within(screen.getByLabelText("Sync shop/production needs approval"));
  expect(card.queryByText("Saves")).toBeNull();
  expect(card.queryByText(/redis/)).toBeNull();
  expect(card.getByText(line(/web .*1 → 2/))).toBeTruthy();
});

it("leaves the asker's own card unsigned, waiting on someone who can protect it", () => {
  show(asTestDouble<ApprovalView>()({ ...approval, protected: true, yours: true, asker: "Linus Torvalds" }));
  const card = within(screen.getByLabelText("Deploy shop/production needs approval"));
  expect(card.queryByText(/Asked by/)).toBeNull();
  expect(card.getByText("Waiting on an owner or admin.")).toBeTruthy();
  expect(card.queryByRole("button", { name: /Approve/ })).toBeNull();
});

it("says how long ago it was asked, so a stale ask reads apart from a fresh one", () => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date("2026-10-08T12:09:00.000Z"));
  try {
    show(asTestDouble<ApprovalView>()({ ...approval, yours: false, asker: "Linus Torvalds" }));
    show(asTestDouble<ApprovalView>()({ ...approval, id: "a2", created_at: "2026-10-08T12:08:30.000Z", yours: true, asker: null }));
    const ages = [...document.querySelectorAll("[data-slot=card-description]")].map((line) => line.textContent);
    expect(ages).toEqual(["Asked by Linus Torvalds · 9 minutes ago", "30 seconds ago"]);
  } finally {
    vi.useRealTimers();
  }
});

it("says a Deploy that prunes removes any other service, though it names none", () => {
  const pruning = { ...approval, review: { ...approval.review, effects: [], prunes: true as const } };
  const card = show(pruning).card();
  expect(within(card).getByText("Removes other services")).toBeTruthy();
  expect(within(card).getByRole("list", { name: "Destroys" }).textContent).toBe("Also removes any other service running in this environment");
});

it("says which servers a pruning Deploy couldn't check because large packets to them are dropped", () => {
  const pruning = { ...approval, review: { ...approval.review, effects: [], prunes: true as const, dropped: ["machine-3"] } };
  const card = show(pruning).card();
  expect(within(card).getByText("Couldn't check every server: large packets to machine-3 are dropped on the mesh path")).toBeTruthy();
});
