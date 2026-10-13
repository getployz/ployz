import type { PloyzInngest } from "#/modules/inngest/client";
import { createVolumeSnapshotScheduledEvent, volumeSnapshotScheduledEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";
import { tickOf } from "./snapshot-schedule";
import { scheduledVolumes, snapshotOnTick } from "./snapshot-schedule.server";

type EffectRunner = typeof runInngestEffect;

/** Each hour, one event per Volume whose schedule is due; the event id carries the tick, so a retry sends nothing new. */
export const createScheduleVolumeSnapshots = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "schedule-volume-snapshots", retries: 1, triggers: [{ cron: "TZ=UTC 0 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const tick = await step.run("tick", () => tickOf(new Date()).toISOString());
      const volumes = await step.run("list-due-volumes", () => runEffect(scheduledVolumes(new Date(tick))));
      if (volumes.length > 0) {
        await step.sendEvent("request-volume-snapshots", volumes.map((volume) => createVolumeSnapshotScheduledEvent({ ...volume, tick })));
      }
      return { tick, volumes: volumes.length };
    },
  );

export const createSnapshotVolume = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect, onTick = snapshotOnTick) =>
  inngest.createFunction(
    {
      id: "snapshot-volume",
      retries: 2,
      triggers: [{ event: volumeSnapshotScheduledEventType }],
      concurrency: [{ key: "event.data.volumeId", limit: 1 }],
    },
    async ({ event, step }) => {
      const { tick, ...volume } = event.data;
      return await step.run("snapshot-and-prune", () => runEffect(onTick(volume, new Date(tick))));
    },
  );

export const createSnapshotScheduleFunctions = (inngest: PloyzInngest) => [createScheduleVolumeSnapshots(inngest), createSnapshotVolume(inngest)];
