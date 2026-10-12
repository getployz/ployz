import { createRequire } from "node:module";
import type * as PloyzSdk from "@ployz/sdk";
import type { AgentTurn, Answer, Ask, Step, ToolResult, Write } from "@ployz/sdk";
import { Effect } from "effect";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { agentResume, agentTools } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "agentResume" | "agentTools">;

/** The Store source the sidebar's calls are recorded under. */
export const AGENT = { source: "agent" } as const;

export const CATALOG = agentTools();

export const GATED = new Set(CATALOG.tools.flatMap((tool) => tool.gate === "never" ? [] : [tool.name]));

type Paused = { step: Extract<Step, { step: "write" | "done" }>; state: AgentTurn["state"] };

export const untilWrite = <E, R>(turn: AgentTurn, read: (ask: Ask) => Effect.Effect<Answer, E, R>) =>
  Effect.gen(function* (): Effect.fn.Return<Paused, E, R> {
    let { step, state } = turn;
    for (;;) {
      if (step.step === "write" || step.step === "done") return { step, state };
      if (step.step === "wait") yield* Effect.sleep(step.ms);
      const answers = yield* Effect.all((step.step === "read" ? step.asks : step.then).map(read), { concurrency: "unbounded" });
      ({ step, state } = agentResume(state, answers));
    }
  });

export const finish = <E, R>(
  turn: AgentTurn,
  read: (ask: Ask) => Effect.Effect<Answer, E, R>,
  write: (write: Write) => Effect.Effect<Answer, E, R>,
) => Effect.gen(function* (): Effect.fn.Return<ToolResult, E, R> {
  let next = turn;
  for (;;) {
    const { step, state } = yield* untilWrite(next, read);
    if (step.step === "done") return step.result;
    next = agentResume(state, [yield* write(step.write)]);
  }
});
