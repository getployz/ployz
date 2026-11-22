import type { StepTools } from "#/modules/inngest/client";

export const PRUNE_BATCH_ROWS = 5000;

/** Uptime prunes 4 kinds in one run: at most 200 batches each stays under Inngest's 1,000 steps per run. The rest waits for the next run. */
const MAX_PRUNE_BATCHES = 200;

/** Delete in batches, one step each, until a batch comes back short. `prune` deletes one batch and returns how many rows went. */
export async function pruneInBatches(step: StepTools<"run">, stepId: string, prune: () => Promise<number>) {
  for (let batch = 0; batch < MAX_PRUNE_BATCHES; batch += 1) {
    const deleted = await step.run(stepId, prune);
    if (deleted < PRUNE_BATCH_ROWS) break;
  }
}
