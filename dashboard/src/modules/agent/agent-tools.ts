import { type AgentTurn, type Answer, agentResume, agentStart, agentTools, type Ask, type Step, type ToolResult, type Write } from "@ployz/sdk";
import { Effect } from "effect";
import { COMMANDS } from "#/modules/commands/registry.server";

/** The Store source the sidebar's calls are recorded under. */
export const AGENT = { source: "agent" } as const;

export const CATALOG = agentTools();

// The compiled tools' turn steps, for the runner, which imports only types from the SDK.
export { agentResume, agentStart };

/** Every tool whose write may wait on a human: the compiled catalog's and Cloud's published commands'. */
export const GATED = new Set([
  ...CATALOG.tools.flatMap((tool) => tool.gate === "never" ? [] : [tool.name]),
  ...COMMANDS.flatMap((command) => command.gate === "never" ? [] : [command.name]),
]);

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
