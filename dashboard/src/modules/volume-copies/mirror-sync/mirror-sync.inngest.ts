import type { PloyzInngest } from "#/modules/inngest/client";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";
import { MIRROR_ROUND_MINUTES } from "#/modules/volume-copies/mirror-sync/mirror-sync.words";
import { refreshMirrors } from "#/modules/volume-copies/mirror-sync/mirror-sync.round.server";
import { organizationsWithMachines } from "#/modules/volume-run/volume-run.orphans.server";

/** Every `MIRROR_ROUND_MINUTES` each organization's mirrors refresh, and a primary that came back is demoted to a detached copy. */
export const createRefreshVolumeMirrors = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "refresh-volume-mirrors", retries: 1, triggers: [{ cron: `*/${MIRROR_ROUND_MINUTES} * * * *` }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const organizations = await step.run("list-organizations", () => runEffect(organizationsWithMachines()));
      let synced = 0;
      for (const organizationId of organizations) {
        synced += (await step.run(`refresh-${organizationId}`, () => runEffect(refreshMirrors(organizationId, null)))).length;
      }
      return { synced };
    },
  );
