import { RuleTester } from "oxlint/plugins-dev";
import { describe, it } from "vitest";

import { uniqueStepIdPerIterationRule } from "./unique-step-id-per-iteration.ts";

RuleTester.describe = describe;
RuleTester.it = it;

const tester = new RuleTester();

// 7540640106, trimmed: finishRelease looked up its target under one id on every check, so each check replayed the first's.
const finishRelease = (targetId: string) => `async function finishRelease(steps, current) {
  const { step, runEffect, run, runId } = steps;
  for (let check = 0; ; check += 1) {
    const members = await step.run(check === 0 ? "01-observe" : \`01-observe-\${check}\`, () => runEffect(observeReachable(run, runId)));
    const to = members?.find((member) => member.machine.id === current)?.machine
      ?? { id: current, name: (await step.run(${targetId}, () => runEffect(laneTarget(run)))) ?? current };
    const forward = members === null ? null : planForwardRelease(run.volumeName, to, members);
    if (forward?.kind === "blocked") return fail(steps, "02-failed", forward.message);
    if (members === null || forward === null || forward.kind === "waiting") {
      await step.sleep(\`01-wait-\${check}\`, "10s");
      continue;
    }
    const { epoch } = await step.run("02-epoch", () => runEffect(takeEpoch(run, runId, observedEpoch(members))));
    return finish(steps, epoch);
  }
}`;

tester.run("unique-step-id-per-iteration", uniqueStepIdPerIterationRule, {
  valid: [
    { code: finishRelease('check === 0 ? "02-target" : `02-target-${check}`') },
    {
      code: `for (const { container } of here) {
        await step.run(\`stop:\${machine}:\${container}\`, () => runEffect(actOnContainer(request, runId, container, "stop")));
      }`,
    },
    {
      code: `for (let attempt = 1; ; attempt += 1) {
        const { left } = await step.run(\`retire-\${attempt}\`, () => runEffect(retireOldCopies(data)));
        if (left.length === 0) {
          await step.run("end", () => runEffect(endCleanup(runId)));
          return;
        }
      }`,
    },
  ],
  invalid: [
    { code: finishRelease('"02-target"'), errors: [{ messageId: "sharedId" }] },
    {
      code: `await Promise.all(machines.map((machine) => steps.step.run(\`stop-\${run.id}\`, () => stop(machine))));`,
      errors: [{ messageId: "sharedId" }],
    },
    // A return in an iterator callback ends one invocation; every machine still runs the step.
    {
      code: `await Promise.all(machines.map((m) => { return step.run("stop", () => stop(m)); }));`,
      errors: [{ messageId: "sharedId" }],
    },
    {
      code: `machines.forEach(async (m) => {
        if (m.gone) return;
        await step.run("stop", () => stop(m));
        return;
      });`,
      errors: [{ messageId: "sharedId" }],
    },
  ],
});
