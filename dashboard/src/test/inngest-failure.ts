import type { EventPayload, FailureEventArgs } from "inngest";

type FailureHandler<Tools, Result> = { readonly opts: { readonly onFailure?: (ctx: FailureEventArgs & Tools) => Result } };

/** Runs a function's `onFailure` handler as Inngest would once run `runId`, triggered by `event`, exhausts its retries. */
export async function invokeOnFailure<Tools, Result>(fn: FailureHandler<Tools, Result>, runId: string, event: EventPayload) {
  const onFailure = fn.opts.onFailure;
  if (onFailure === undefined) throw new Error("the function has no onFailure handler");
  const error = new Error("the run failed");
  const failure: FailureEventArgs = {
    event: {
      name: "inngest/function.failed",
      data: { function_id: "test", run_id: runId, error: { name: error.name, message: error.message }, event },
    },
    error,
  };
  // SAFETY: failure handlers read only the failed run's event and error, never its step or group tools.
  return onFailure(failure as FailureEventArgs & Tools);
}
