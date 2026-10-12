import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import type { UserSnapshot, VolumeSnapshots, WantedSnapshot } from "@ployz/sdk";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { DEFAULT_POLICY, type SnapshotPolicy, scheduledSnapshotId } from "./snapshot-schedule";
import { type ScheduledVolume, snapshotAndPrune } from "./snapshot-schedule.server";
import { createSnapshotVolume } from "./snapshot-schedule.inngest";
import type { SnapshotsFailure, SnapshotsResult } from "./volume-snapshots.server";

const TICK = "2026-10-12T13:00:00.000Z";
const VOLUME = { organizationId: "org-1", environment: { project: "shop", environment: "production" }, volumeId: "vol-1" };
const POLICY: SnapshotPolicy = { ...DEFAULT_POLICY, every: "hour", keep: 1 };
const TAKE_ID = scheduledSnapshotId("hour", VOLUME.volumeId, new Date(TICK));

const wire = (id: string, minutesAgo: number): UserSnapshot => ({
  id, label: { kind: "hourly", note: "" }, kept: false, created_unix_seconds: Date.parse(TICK) / 1000 - minutesAgo * 60,
  written_bytes: 1e6, referenced_bytes: 1e9, loses_bytes: 0,
});
const listed = (epoch: number, snapshots: UserSnapshot[]): SnapshotsResult => ({ ok: true, snapshots: { epoch, snapshots } satisfies VolumeSnapshots });
const refused = (reason: SnapshotsFailure): SnapshotsResult => ({ ok: false, reason, refusal: { code: "conflict", message: String(reason), details: null } });

/** A writer that answers each list and set from a script, and records every set it was sent. */
function scriptedWriter(lists: SnapshotsResult[], sets: ((want: WantedSnapshot) => SnapshotsResult)[]) {
  const sent: { expect: number; want: WantedSnapshot }[] = [];
  const writer = {
    list: Effect.sync(() => lists.shift() ?? refused(null)),
    set: (expect: number, want: WantedSnapshot) => Effect.sync(() => {
      sent.push({ expect, want });
      return (sets.shift() ?? (() => refused(null)))(want);
    }),
  };
  return { writer, sent };
}

const runner = makeInngestEffectRunner(<A, E>(effect: Effect.Effect<A, E, never>) => Effect.runPromise(effect)) as typeof runInngestEffect;

async function runTick(writer: ReturnType<typeof scriptedWriter>["writer"]) {
  const onTick = (_volume: ScheduledVolume, tick: Date) =>
    snapshotAndPrune(writer, POLICY, { id: scheduledSnapshotId("hour", VOLUME.volumeId, tick), label: { kind: "hourly", note: "" } });
  const output = await new InngestTestEngine({
    function: createSnapshotVolume(new Inngest({ id: "test" }), runner, onTick),
    events: [{ name: "volume/snapshot.scheduled", data: { ...VOLUME, tick: TICK } }],
  }).execute();
  return output.result;
}

describe("snapshot-volume", () => {
  it("takes the tick's snapshot and prunes the hourly one past keep", async () => {
    const older = wire("hourly-old", 60);
    const { writer, sent } = scriptedWriter([listed(7, [older])], [
      () => listed(7, [wire(TAKE_ID, 0), older]),
      () => listed(7, [wire(TAKE_ID, 0)]),
    ]);
    expect(await runTick(writer)).toEqual({ status: "done", taken: TAKE_ID, deleted: ["hourly-old"] });
    expect(sent.map((entry) => entry.want.kind)).toEqual(["taken", "deleted"]);
  });

  it("does not take the snapshot again when a retried tick finds it", async () => {
    const { writer, sent } = scriptedWriter([listed(7, [wire(TAKE_ID, 0)])], []);
    expect(await runTick(writer)).toEqual({ status: "done", taken: TAKE_ID, deleted: [] });
    expect(sent).toEqual([]);
  });

  it.each(["busy", "no_writer"] as const)("skips the tick when the writer answers %s", async (reason) => {
    const { writer, sent } = scriptedWriter([listed(7, [wire("hourly-old", 60)])], [() => refused(reason)]);
    expect(await runTick(writer)).toEqual({ status: "skipped", reason, message: reason });
    expect(sent).toHaveLength(1);
  });

  it("reports a prune the writer refuses as skipped, not done", async () => {
    const older = wire("hourly-old", 60);
    const { writer, sent } = scriptedWriter([listed(7, [older])], [
      () => listed(7, [wire(TAKE_ID, 0), older]),
      () => refused("busy"),
    ]);
    expect(await runTick(writer)).toEqual({ status: "skipped", reason: "busy", message: "busy" });
    expect(sent.map((entry) => entry.want.kind)).toEqual(["taken", "deleted"]);
  });

  it("skips a Volume whose writer lacks the snapshot capabilities, sending nothing", async () => {
    const { writer, sent } = scriptedWriter([refused("unsupported")], []);
    expect(await runTick(writer)).toMatchObject({ status: "skipped", reason: "unsupported" });
    expect(sent).toEqual([]);
  });

  it("reads the epoch again once on a mismatch and takes the snapshot at the new epoch", async () => {
    const { writer, sent } = scriptedWriter([listed(7, []), listed(8, [])], [
      () => refused("mismatch"),
      () => listed(8, [wire(TAKE_ID, 0)]),
    ]);
    expect(await runTick(writer)).toEqual({ status: "done", taken: TAKE_ID, deleted: [] });
    expect(sent.map((entry) => entry.expect)).toEqual([7, 8]);
  });

  it("skips after a second mismatch rather than reading again", async () => {
    const { writer, sent } = scriptedWriter([listed(7, []), listed(8, []), listed(9, [])], [
      () => refused("mismatch"),
      () => refused("mismatch"),
    ]);
    expect(await runTick(writer)).toMatchObject({ status: "skipped", reason: "mismatch" });
    expect(sent).toHaveLength(2);
  });
});
