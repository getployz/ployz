import { readFileSync } from "node:fs";
import { type Answer, type JsonValue, type Place, type Step, agentResume, agentStart, agentTools } from "@ployz/sdk";
import { describe, expect, it } from "vitest";

type Transcript = { tool: string; input: JsonValue; place: Place; call_key: string; turns: { step: Step; answers?: Answer[] }[] };

const snapshot = <T>(name: string): T =>
  JSON.parse(readFileSync(new URL(`../../../../core/crates/ployz-store/tests/snapshots/${name}`, import.meta.url), "utf8")) as T;

const replay = ({ tool, input, place, call_key, turns }: Transcript): Step[] => {
  let { step, state } = agentStart(tool, input, place, call_key);
  const steps: Step[] = [];
  for (const turn of turns) {
    steps.push(step);
    if (step.step === "done") break;
    ({ step, state } = agentResume(state, turn.answers ?? []));
  }
  return steps;
};

describe("agent steps over the Node SDK", () => {
  it("offers the catalog Rust snapshots", () => {
    expect(agentTools()).toEqual(snapshot("agent_tools.json"));
  });

  const transcripts = snapshot<Transcript[]>("agent_transcripts.json");

  it("has a transcript for every tool at both places", () => {
    expect(transcripts).toHaveLength(agentTools().tools.length * 2);
  });

  it.each(transcripts.map((transcript) => [`${transcript.tool} at ${transcript.place.store}`, transcript] as const))(
    "%s takes the recorded steps",
    (_, transcript) => {
      expect(replay(transcript)).toEqual(transcript.turns.map((turn) => turn.step));
    },
  );
});
