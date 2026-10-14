import type { PloyzInngest } from "#/modules/inngest/client";
import { refreshMirrors } from "#/modules/volume-placement/volume-placement.deploy.server";
import { organizationsWithMachines } from "#/modules/volume-run/volume-run.server";
import { runInngestEffect } from "#/server/run.server";

type EffectRunner = typeof runInngestEffect;

/** Every 15 minutes each organization's mirrors refresh, and a primary that came back is demoted to a detached copy. */
export const createRefreshVolumeMirrors = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "refresh-volume-mirrors", retries: 1, triggers: [{ cron: "*/15 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const organizations = await step.run("list-organizations", () => runEffect(organizationsWithMachines()));
      let synced = 0;
      for (const organizationId of organizations) {
        synced += (await step.run(`refresh-${organizationId}`, () => runEffect(refreshMirrors(organizationId, null)))).length;
      }
      return { synced };
    },
  );
