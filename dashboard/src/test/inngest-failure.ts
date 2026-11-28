import type { EventPayload, FailureEventArgs } from "inngest";

type FailureHandler<Tools, Result> = { readonly opts: { readonly onFailure?: (ctx: FailureEventArgs & Tools) => Result } };

/** The steps a failure handler runs, each run once, its events collected in `sent`. */
export const failureSteps = () => {
  const sent: Array<EventPayload | EventPayload[]> = [];
  return {
    sent,
    step: {
      run: async <T>(_id: string, fn: () => Promise<T>) => fn(),
      sendEvent: async (_id: string, payload: EventPayload | EventPayload[]) => {
        sent.push(payload);
        return { ids: [] };
      },
    },
  };
};

/** Runs a function's `onFailure` handler as Inngest would once run `runId`, triggered by `event`, exhausts its retries. */
export async function invokeOnFailure<Tools, Result>(
  fn: FailureHandler<Tools, Result>, runId: string, event: EventPayload, { step } = failureSteps(),
) {
  const onFailure = fn.opts.onFailure;
  if (onFailure === undefined) throw new Error("the function has no onFailure handler");
  const error = new Error("the run failed");
  const failed: FailureEventArgs = {
    event: {
      name: "inngest/function.failed",
      data: { function_id: "test", run_id: runId, error: { name: error.name, message: error.message }, event },
    },
    error,
  };
  const failure: FailureEventArgs = Object.assign(failed, { step });
  // SAFETY: failure handlers read only the failed run's event and error, and run and send steps, never other tools.
  return onFailure(failure as FailureEventArgs & Tools);
}
