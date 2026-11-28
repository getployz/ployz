import type { InngestFunction } from "inngest";

/**
 * `fn` as Inngest runs it, with one run id across every step. `InngestTestEngine` draws a fresh id for each replay,
 * which Inngest never does: a function that names its run as the runner of its writes needs the same one each step.
 */
export const stableRunId = <F extends InngestFunction.Any>(fn: F, runId: string): F => {
  const create = fn["createExecution"].bind(fn);
  const stable: F = Object.create(fn);
  stable["createExecution"] = (options: Parameters<typeof create>[0]) => create({
    ...options,
    partialOptions: { ...options.partialOptions, runId, data: { ...options.partialOptions.data, runId } },
  });
  return stable;
};
