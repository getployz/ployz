import type { MachineId, ResolvedServiceSpec, Signed } from "@ployz/sdk";
import { type DraftWorkflow, volumeApprovalBody } from "@ployz/sdk/authority";
import { describe, expect, it } from "vitest";
import type { Member, VolumeRunInput } from "#/modules/volume-run/volume-run";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import { keptFor, type PlacementApproval, placementAsks, seedAsk, stagedAsks, withApproval } from "./placement-approvals";
import { planFromCopies } from "#/modules/volume-run/plan";
import { nextPlacementStep, placementChanges, type PlacementChange, type PlacementRow } from "./volume-placement";

const id = (server: string) => server.padEnd(32, "0") as MachineId;
const ref = (server: string) => ({ id: id(server), name: server });
const volume = { id: "v1", name: "data" };
const RUNNING = { name: "db", container: { image: "postgres@sha256:" + "a".repeat(64) } } as ResolvedServiceSpec;
const terms = { running: RUNNING, recorded: 4_000_000_000 };
/** An approval body with only the fields these tests name: a Volume's workflow, a tag, or padding past the bound. */
type Body = { readonly volume?: string; readonly workflow?: Workflow | DraftWorkflow; readonly for?: string; readonly pad?: string };
type Workflow = { readonly kind: string } & Readonly<Record<string, string>>;
const signed = (body: Body): Signed => ({ body: JSON.stringify(body), signatures: [], assertions: [] });
const row = (staged: PlacementRow["staged"], mirrors: MachineId[] = []): PlacementRow =>
  ({ volumeId: "v1", environmentId: "e1", mirrors, staged, unreached: false, deploymentId: null });

describe("placementAsks", () => {
  const members: Member[] = [member("web-1", "writer"), member("web-2", "mirror"), member("web-3", "old"), member("web-4", "empty")];

  it("drafts a Move to a mirror off the writer, naming the mirrors that keep following it", () => {
    const [move] = placementChanges(volume, [...members, member("web-5", "mirror", { slot: "2" })],
      row({ primary: id("web-2"), mirrors: [id("web-5")], deletes: [] }, [id("web-2"), id("web-5")]));
    expect(move?.kind).toBe("move");
    const asks = placementAsks("data", move as PlacementChange, [...members, member("web-5", "mirror", { slot: "2" })], terms);
    expect(asks).toEqual([expect.objectContaining({
      run: "move",
      machine: id("web-2"),
      change: expect.objectContaining({
        workflow: { kind: "move", from: id("web-1"), to: id("web-2"), mirrors: [id("web-5")], running: RUNNING, recorded: 4_000_000_000 },
        parties: [id("web-1"), id("web-2"), id("web-5")],
      }),
    })]);
  });

  it("drafts a Mirror, a Mirror removal of a slot that holds data, and none for a slot that holds nothing", () => {
    const add: PlacementChange = { kind: "add_mirror", volume, server: ref("web-4"), clears: [ref("web-3")] };
    expect(placementAsks("data", add, members, terms).map((ask) => [ask.run, ask.machine, ask.change.workflow])).toEqual([
      ["delete_mirror", id("web-3"), { kind: "mirror_removal", mirror: id("web-3"), role: "old_copy", writer: id("web-1") }],
      ["mirror", id("web-4"), { kind: "mirror", writer: id("web-1"), mirror: id("web-4"), replace: false }],
    ]);
    const empty: Member[] = [member("web-1", "writer"), member("web-2", "pulling")];
    const nothing = { ...member("web-2", "mirror"), view: { copy: { kind: "slot", slot: "1", readonly: true, newest: null, resume_token: null, pull: null, role: "mirror" } } } as Member;
    expect(placementAsks("data", { kind: "remove_mirror", volume, server: ref("web-2") }, [empty[0] as Member, nothing], terms)).toEqual([]);
  });

  it("names the same loss the person confirmed in the Restore its passkey answers", () => {
    const silent: Member[] = [member("web-1", "unanswered"), member("web-2", "mirror")];
    const [restore] = placementChanges(volume, silent, row({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]));
    expect(restore).toMatchObject({ kind: "move", how: "restore" });
    const [ask] = placementAsks("data", restore as PlacementChange, silent, terms);
    const lostAfter = (restore as Extract<PlacementChange, { kind: "move" }>).lostAfter;
    expect(ask?.change.workflow).toEqual({ kind: "restore", machine: id("web-2"), at: "11", loses_after: lostAfter?.getTime() });
    // The fixture's Server ids aren't the daemon's hex form, so the draft names web-2 by one that is.
    const hex = "b".repeat(32) as MachineId;
    const workflow = ask?.change.workflow.kind === "restore" ? { ...ask.change.workflow, machine: hex } : null;
    if (workflow === null) throw new Error("a Restore");
    const draft = volumeApprovalBody({
      cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=", operation: "0".repeat(32) as never, namespace: "app", volume: "app_vol-v1", name: "data",
      servers: { [hex]: "web-2" }, workflow, issued_at: 1, signer: "passkey",
    });
    expect(draft.lines[0]).toContain(`Writes to data after ${lostAfter?.toISOString().replace(".000", "")} are lost.`);
  });

  it("drafts a restore into a new Volume as a Seed on its source's primary", () => {
    const change: Extract<PlacementChange, { kind: "seed" }> = {
      kind: "seed", volume: { id: "v2", name: "data-2355" }, source: { id: "v1", name: "data" }, to: ref("web-1"),
      dockerVolume: "app_vol-v2", point: { guid: "42", name: "w-1-4", at: 1 },
    };
    expect(seedAsk(change, "app_vol-v1", id("web-1"))).toMatchObject({
      run: "seed", machine: id("web-1"), change: { workflow: { kind: "seed", machine: id("web-1"), source: "app_vol-v1", at: "42" }, parties: [id("web-1")] },
    });
  });
});

describe("stagedAsks", () => {
  const enforcing = (server: string, copy: Parameters<typeof member>[1]): Member => {
    const each = member(server, copy);
    return each.answered ? { ...each, enforces: true, passkey: true } : each;
  };

  // A writer with no snapshot yet still leaves one: the Move hands off at a final snapshot, so its old copy holds data.
  const unsnapshotted = (): Member => {
    const writer = enforcing("web-1", "writer");
    return writer.answered && writer.view.copy?.kind === "root" ? { ...writer, view: { copy: { ...writer.view.copy, newest: null } } } : writer;
  };

  it.each([
    ["a writer with a snapshot", () => enforcing("web-1", "writer")],
    ["a writer with none yet", unsnapshotted],
  ])("drafts what a Move's Deploy runs after it from the copies it leaves, off %s: the old copy cleared, then the mirror fed by the new writer", (_, writer) => {
    const before = [writer(), enforcing("web-2", "empty"), enforcing("web-3", "empty")];
    const staged = row({ primary: id("web-2"), mirrors: [id("web-3")], deletes: [] });
    const asks = stagedAsks(volume, before, staged, terms);
    expect(asks.map(({ run, change }) => ({ run, workflow: change.workflow }))).toEqual([
      { run: "move", workflow: expect.objectContaining({ kind: "move", from: id("web-1"), to: id("web-2") }) },
      { run: "delete_mirror", workflow: { kind: "mirror_removal", mirror: id("web-1"), role: "old_copy", writer: id("web-2") } },
      { run: "mirror", workflow: { kind: "mirror", writer: id("web-2"), mirror: id("web-3"), replace: false } },
    ]);

    // Once the Move lands, each next run the Deploy plans carries its approval and plans to run.
    const kept: PlacementApproval[] = asks.map((ask) =>
      ({ run: ask.run, machine: ask.machine, approval: signed({ volume: "app_vol-v1", workflow: ask.change.workflow }) }));
    const after = [enforcing("web-1", "old"), enforcing("web-2", "writer"), enforcing("web-3", "empty")];
    const next = nextPlacementStep(placementChanges(volume, after, staged), []);
    const run = next === null ? null : withApproval(next.run, kept, after);
    if (run?.kind !== "delete_mirror") throw new Error(`expected the old copy's removal next, got ${run?.kind}`);
    expect(planFromCopies({ kind: run.kind, args: run.args, volumeName: "data", orphan: false }, after).ok).toBe(true);
  });

  it("drafts a staged change with no Move over the copies as they stand", () => {
    const members = [member("web-1", "writer"), member("web-2", "empty")];
    const asks = stagedAsks(volume, members, row({ primary: id("web-1"), mirrors: [id("web-2")], deletes: [] }), terms);
    expect(asks.map(({ change }) => change.workflow)).toEqual([{ kind: "mirror", writer: id("web-1"), mirror: id("web-2"), replace: false }]);
  });
});

describe("withApproval", () => {
  const members: Member[] = [member("web-1", "writer"), member("web-2", "mirror")];
  const kept: PlacementApproval[] = [
    { run: "move", machine: id("web-2"), approval: signed({ for: "move" }), service: RUNNING },
    { run: "delete_mirror", machine: id("web-1"), approval: signed({ for: "delete" }) },
  ];

  it("carries the approval kept for the run's kind and Server, and nothing to another", () => {
    const move: VolumeRunInput = { kind: "move", args: { to: "web-2" } };
    expect(withApproval(move, kept, members)).toEqual({ kind: "move", args: { to: "web-2", approval: kept[0]?.approval, service: RUNNING } });
    const other: VolumeRunInput = { kind: "move", args: { to: "web-3" } };
    expect(withApproval(other, kept, members)).toEqual(other);
    const removal: VolumeRunInput = { kind: "delete_mirror", args: { slot: "web-2", confirmed_name: null, machine: id("web-2"), role: "mirror" } };
    expect(withApproval(removal, kept, members)).toEqual(removal);
  });
});

describe("keptFor", () => {
  const body = (workflow: Workflow, volume = "app_vol-v1") => signed({ volume, workflow });

  it("keeps only an approval shaped for the run and Server it names, on this Volume", () => {
    const move = { run: "move" as const, machine: id("web-2"), service: RUNNING };
    expect(keptFor({ ...move, approval: body({ kind: "move", to: id("web-2") }) }, "app_vol-v1")).toBe(true);
    expect(keptFor({ ...move, approval: body({ kind: "move", to: id("web-3") }) }, "app_vol-v1")).toBe(false);
    expect(keptFor({ ...move, approval: body({ kind: "move", to: id("web-2") }, "app_vol-v9") }, "app_vol-v1")).toBe(false);
    expect(keptFor({ run: "move", machine: id("web-2"), approval: body({ kind: "move", to: id("web-2") }) }, "app_vol-v1")).toBe(false);
    expect(keptFor({ run: "mirror", machine: id("web-2"), approval: body({ kind: "restore", machine: id("web-2") }) }, "app_vol-v1")).toBe(false);
    expect(keptFor({ run: "delete_mirror", machine: id("web-2"), approval: body({ kind: "mirror_removal", mirror: id("web-2") }) }, "app_vol-v1")).toBe(true);
    expect(keptFor({ run: "seed", machine: id("web-1"), approval: signed({ volume: "app_vol-v1", workflow: { kind: "seed", machine: id("web-1") }, pad: "x".repeat(5000) }) }, "app_vol-v1")).toBe(false);
  });
});
