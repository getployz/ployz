import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { createVolumeOffsiteScheduledEvent } from "#/modules/inngest/events";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { createOffsiteVolume } from "./volume-offsite.inngest";
import type { DailyPass, OffsiteVolume } from "./volume-offsite.server";

const TICK = "2026-10-12T00:00:00.000Z";
const VOLUME = { organizationId: "org-1", environment: { project: "shop", environment: "production" }, volumeId: "vol-1" };

const runner = makeInngestEffectRunner(<A, E>(effect: Effect.Effect<A, E, never>) => Effect.runPromise(effect)) as typeof runInngestEffect;

/** Answers each upload ask from a script and records what it was asked with. */
function scripted(passes: DailyPass[], deleted: string[] = ["off-old"]) {
  const asked: { volume: OffsiteVolume; tick: string; from: string | null }[] = [];
  let pruned = 0;
  const work = {
    dailyUpload: (volume: OffsiteVolume, tick: Date, from: string | null) => Effect.sync(() => {
      asked.push({ volume, tick: tick.toISOString(), from });
      return passes.shift() ?? ({ status: "skipped", message: "script ran out" } satisfies DailyPass);
    }),
    pruneOffsite: () => Effect.sync(() => {
      pruned++;
      return deleted;
    }),
  };
  return { work, asked, pruned: () => pruned };
}

async function runDay(work: ReturnType<typeof scripted>["work"]) {
  const output = await new InngestTestEngine({
    function: createOffsiteVolume(new Inngest({ id: "test" }), runner, work),
    events: [{ name: "volume/offsite.scheduled", data: { ...VOLUME, tick: TICK } }],
    steps: Array.from({ length: 5 }, (_, ask) => ({ id: `upload-wait-${ask + 1}`, handler: () => undefined })),
  }).execute();
  return output.result;
}

describe("offsite-volume", () => {
  it("asks again with the source the first ask took until the upload settles, then prunes past keep", async () => {
    const { work, asked, pruned } = scripted([{ status: "running", from: "g1" }, { status: "settled", failed: null }]);
    expect(await runDay(work)).toEqual({ deleted: ["off-old"] });
    expect(asked.map((entry) => entry.from)).toEqual([null, "g1"]);
    expect(asked.every((entry) => entry.tick === TICK && entry.volume.volumeId === "vol-1")).toBe(true);
    expect(pruned()).toBe(1);
  });

  it("still prunes after a failed upload, since older backups stay valid", async () => {
    const { work, pruned } = scripted([{ status: "settled", failed: "bucket full" }]);
    expect(await runDay(work)).toEqual({ deleted: ["off-old"] });
    expect(pruned()).toBe(1);
  });

  it("skips the day on a refusal or a writer without offsite, and prunes nothing", async () => {
    const { work, pruned } = scripted([{ status: "skipped", message: "Offsite needs an upgrade on web-1" }]);
    expect(await runDay(work)).toEqual({ upload: { status: "skipped", message: "Offsite needs an upgrade on web-1" }, deleted: [] });
    expect(pruned()).toBe(0);
  });
});

describe("volume/offsite.scheduled", () => {
  it("is keyed by the Volume and the day, so a retried schedule sends nothing new", () => {
    const event = createVolumeOffsiteScheduledEvent({ ...VOLUME, tick: TICK });
    expect(event.id).toBe("volume-offsite-vol-1-1791763200");
    expect(createVolumeOffsiteScheduledEvent({ ...VOLUME, tick: TICK }).id).toBe(event.id);
    expect(JSON.stringify(event)).not.toMatch(/secret|access/i);
  });
});
