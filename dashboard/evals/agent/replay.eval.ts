import { readdirSync, readFileSync } from "node:fs";
import { describe, it } from "@effect/vitest";
import { Effect, Schema } from "effect";
import { expect } from "vitest";
import { Golden, Replayer, replaying } from "./tape";
import { TASKS } from "./tasks";
import { runTrial } from "./trial";

const GOLDEN = new URL("./golden/", import.meta.url);
const goldens = readdirSync(GOLDEN).filter((file) => file.endsWith(".json"))
  .map((file) => Schema.decodeUnknownSync(Schema.fromJsonString(Golden))(readFileSync(new URL(file, GOLDEN), "utf8")));

describe.skipIf(goldens.length === 0)("golden trajectories", () => {
  for (const golden of goldens) {
    it.live(`${golden.task} still passes as ${golden.model} played it`, () => Effect.gen(function* () {
      const task = TASKS.find(({ id }) => id === golden.task);
      expect(task, `no task ${golden.task}`).toBeDefined();
      if (task === undefined) return;
      const { failures } = yield* runTrial(task, new Replayer(golden), replaying(golden.member));
      expect(failures).toEqual([]);
    }));
  }
});
