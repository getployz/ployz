import type { PloyzInngest } from "#/modules/inngest/client";
import { createVolumeOffsiteScheduledEvent, volumeOffsiteScheduledEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";
import { tickOf } from "./snapshot-schedule";
import { dailyUpload, offsiteVolumes, pruneOffsite } from "./volume-offsite.server";

type EffectRunner = typeof runInngestEffect;

const ASK_AGAIN = "1m";
const MAX_ASKS = 360;

/** Each UTC midnight, one event per Volume with an offsite copy; the event id carries the day, so a retry sends nothing new. */
export const createScheduleVolumeOffsite = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "schedule-volume-offsite", retries: 1, triggers: [{ cron: "TZ=UTC 0 0 * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const tick = await step.run("tick", () => tickOf(new Date()).toISOString());
      const volumes = await step.run("list-offsite-volumes", () => runEffect(offsiteVolumes()));
      if (volumes.length > 0) {
        await step.sendEvent("request-volume-offsite", volumes.map((volume) => createVolumeOffsiteScheduledEvent({ ...volume, tick })));
      }
      return { tick, volumes: volumes.length };
    },
  );

/**
 * The day's upload, asked again with the same source until the writer settles it, then the prune past `keep`. Steps
 * return only statuses: the bucket and its secret are read and decrypted inside each step.
 */
export const createOffsiteVolume = (
  inngest: PloyzInngest,
  runEffect: EffectRunner = runInngestEffect,
  work = { dailyUpload, pruneOffsite },
) =>
  inngest.createFunction(
    {
      id: "offsite-volume",
      retries: 2,
      triggers: [{ event: volumeOffsiteScheduledEventType }],
      concurrency: [{ key: "event.data.volumeId", limit: 1 }],
    },
    async ({ event, step }) => {
      const { tick, ...volume } = event.data;
      let from: string | null = null;
      for (let ask = 0; ask < MAX_ASKS; ask++) {
        if (ask > 0) await step.sleep(`upload-wait-${ask}`, ASK_AGAIN);
        const pass = await step.run(`upload-${ask}`, () => runEffect(work.dailyUpload(volume, new Date(tick), from)));
        if (pass.status !== "running") {
          if (pass.status === "skipped") return { upload: pass, deleted: [] };
          break;
        }
        from = pass.from;
      }
      const deleted = await step.run("prune", () => runEffect(work.pruneOffsite(volume)));
      return { deleted };
    },
  );

export const createOffsiteScheduleFunctions = (inngest: PloyzInngest) => [createScheduleVolumeOffsite(inngest), createOffsiteVolume(inngest)];
