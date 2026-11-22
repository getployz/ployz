import { Effect } from "effect";
import { vi } from "vitest";
import * as inngestRunner from "#/modules/inngest/run.server";
import * as runner from "#/server/run.server";

export function useServiceFreeEffectRunner() {
  const run = runner.makeEffectRunner({ runPromiseExit: Effect.runPromiseExit });
  // SAFETY: Callers replace their service operations with effects requiring no services.
  vi.spyOn(runner, "runAppEffect").mockImplementation(run as typeof runner.runAppEffect);
  // SAFETY: Preserve the real Inngest error encoder around the same service-free effects.
  vi.spyOn(inngestRunner, "runInngestEffect").mockImplementation(
    inngestRunner.makeInngestEffectRunner(run) as typeof inngestRunner.runInngestEffect,
  );
}
