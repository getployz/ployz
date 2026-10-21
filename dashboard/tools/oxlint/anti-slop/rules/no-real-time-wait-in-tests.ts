import { defineRule } from "@oxlint/plugins";

import type { Comment, ESTree } from "@oxlint/plugins";

const testFilePattern = /\.test\.[cm]?[jt]sx?$/u;
const timerPromiseModules = new Set(["node:timers/promises", "timers/promises"]);
const timerFunctions = new Set(["setTimeout", "setInterval", "setImmediate"]);
const scheduleWaits = new Set(["spaced", "fixed", "exponential"]);
const timerGlobals = new Set(["globalThis", "global", "window"]);
const testerRoots = new Set(["it", "test"]);
const fakeTimerHooks = new Set(["beforeEach", "beforeAll"]);
const suppression = /(?:es|ox)lint-disable(?:-next-line|-line)?\b[^\n]*anti-slop\/no-real-time-wait-in-tests/u;
const suppressionReason = /\s--\s*\S/u;

type Tester = "effect" | "live" | "plain";
type Fn = ESTree.ArrowFunctionExpression | ESTree.Function;

interface CalleeChain {
  readonly root: string;
  readonly members: ReadonlyArray<string>;
  readonly calls: ReadonlyArray<ESTree.CallExpression>;
}

interface FakeTimers {
  readonly call: ESTree.CallExpression;
  readonly fakes: ReadonlySet<string>;
}

function isFunction(node: ESTree.Node): node is Fn {
  return (
    node.type === "ArrowFunctionExpression" ||
    node.type === "FunctionDeclaration" ||
    node.type === "FunctionExpression"
  );
}

function propertyName(node: ESTree.Node): string | null {
  if (node.type === "Identifier") return node.name;
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  return null;
}

function calleeChain(callee: ESTree.Node): CalleeChain | null {
  const members: Array<string> = [];
  const calls: Array<ESTree.CallExpression> = [];
  let current: ESTree.Node = callee;
  while (true) {
    if (current.type === "Identifier") return { root: current.name, members: members.toReversed(), calls };
    if (current.type === "CallExpression") {
      calls.push(current);
      current = current.callee;
    } else if (current.type === "MemberExpression") {
      const name = propertyName(current.property);
      if (name === null) return null;
      members.push(name);
      current = current.object;
    } else {
      return null;
    }
  }
}

function memberCall(callee: ESTree.Node): readonly [string, string] | null {
  if (callee.type !== "MemberExpression" || callee.object.type !== "Identifier") return null;
  const property = propertyName(callee.property);
  return property === null ? null : [callee.object.name, property];
}

function isLayerBlock(chain: CalleeChain): boolean {
  return chain.root === "layer" || (testerRoots.has(chain.root) && chain.members.includes("layer"));
}

function testerKind(chain: CalleeChain): Tester | null {
  if (!testerRoots.has(chain.root) || chain.members.includes("layer")) return null;
  if (chain.members.includes("live") || chain.members.includes("scopedLive")) return "live";
  if (chain.members.includes("effect") || chain.members.includes("scoped")) return "effect";
  return "plain";
}

function objectArguments(call: ESTree.CallExpression): Array<ESTree.ObjectExpression> {
  return call.arguments.filter((argument) => argument.type === "ObjectExpression");
}

function literalProperty(object: ESTree.ObjectExpression, key: string): ESTree.Expression | null {
  for (const property of object.properties) {
    if (property.type === "Property" && propertyName(property.key) === key) return property.value;
  }
  return null;
}

function excludesTestServices(chain: CalleeChain): boolean {
  return chain.calls.some((call) =>
    objectArguments(call).some((object) => {
      const value = literalProperty(object, "excludeTestServices");
      return value?.type === "Literal" && value.value === true;
    }),
  );
}

function enclosingTester(node: ESTree.Node): Tester | null {
  let child: ESTree.Node = node;
  let current: ESTree.Node | null = node.parent;
  let tester: Tester | null = null;
  while (current !== null) {
    if (current.type === "CallExpression" && current.arguments.some((argument) => argument === child)) {
      const chain = calleeChain(current.callee);
      if (chain !== null) {
        if (tester === null) {
          tester = testerKind(chain);
          if (tester !== null && tester !== "effect") return tester;
        } else if (isLayerBlock(chain) && excludesTestServices(chain)) {
          return "live";
        }
      }
    }
    child = current;
    current = current.parent;
  }
  return tester;
}

function enclosingTestCall(node: ESTree.Node): ESTree.CallExpression | null {
  let child: ESTree.Node = node;
  for (let current = node.parent; current !== null; current = current.parent) {
    if (current.type === "CallExpression" && current.arguments.some((argument) => argument === child)) {
      const chain = calleeChain(current.callee);
      if (chain !== null && testerKind(chain) !== null) return current;
    }
    child = current;
  }
  return null;
}

function isInsidePromiseExecutor(node: ESTree.Node): boolean {
  let current: ESTree.Node | null = node.parent;
  while (current !== null) {
    if (isFunction(current)) {
      const parent = current.parent;
      return (
        parent?.type === "NewExpression" &&
        parent.arguments[0] === current &&
        parent.callee.type === "Identifier" &&
        parent.callee.name === "Promise"
      );
    }
    current = current.parent;
  }
  return false;
}

// `vi.useFakeTimers()` fakes every timer; `toFake` narrows it, and `shouldAdvanceTime`
// lets fake time follow the wall clock, so the test still waits for real.
function fakedTimers(call: ESTree.CallExpression): ReadonlySet<string> {
  const options = objectArguments(call)[0];
  if (options === undefined) return timerFunctions;
  const advances = literalProperty(options, "shouldAdvanceTime");
  if (advances?.type === "Literal" && advances.value === true) return new Set();
  const toFake = literalProperty(options, "toFake");
  if (toFake?.type !== "ArrayExpression") return timerFunctions;
  return new Set(
    toFake.elements.flatMap((element) =>
      element?.type === "Literal" && typeof element.value === "string" ? [element.value] : [],
    ),
  );
}

function isAncestor(ancestor: ESTree.Node, node: ESTree.Node): boolean {
  return ancestor.start <= node.start && node.end <= ancestor.end;
}

function enclosingFunctions(node: ESTree.Node): Array<Fn> {
  const functions: Array<Fn> = [];
  for (let current = node.parent; current !== null; current = current.parent) {
    if (isFunction(current)) functions.push(current);
  }
  return functions;
}

function hookScope(fakes: FakeTimers): ESTree.Node | null {
  const [hookCallback] = enclosingFunctions(fakes.call);
  const hook = hookCallback?.parent;
  if (hook?.type !== "CallExpression" || hook.callee.type !== "Identifier") return null;
  if (!fakeTimerHooks.has(hook.callee.name)) return null;
  return hook.parent?.parent ?? null;
}

function restoredBefore(node: ESTree.Node, fakes: FakeTimers, restores: ReadonlyArray<ESTree.CallExpression>): boolean {
  return restores.some((restore) => {
    if (restore.start < fakes.call.end || node.start < restore.end) return false;
    const [owner] = enclosingFunctions(restore);
    return owner === undefined || isAncestor(owner, node);
  });
}

function runsUnderFakeTimers(
  node: ESTree.CallExpression,
  timer: string,
  fakeTimers: ReadonlyArray<FakeTimers>,
  restores: ReadonlyArray<ESTree.CallExpression>,
): boolean {
  return fakeTimers.some((fakes) => {
    if (!fakes.fakes.has(timer) || restoredBefore(node, fakes, restores)) return false;
    const scope = hookScope(fakes);
    if (scope !== null) return isAncestor(scope, node);
    const [owner] = enclosingFunctions(fakes.call);
    return owner !== undefined && isAncestor(owner, node) && fakes.call.end <= node.start;
  });
}

export const noRealTimeWaitInTestsRule = defineRule({
  meta: {
    type: "problem",
    docs: {
      description:
        "Disallow real-clock waits in tests; await the milestone, poll its state with `vi.waitFor`, or drive fake time instead.",
    },
    messages: {
      realClock:
        "Real-clock wait in a test. Await the promise, `Deferred`, `Queue` or event that marks the milestone; poll state with no push signal through `vi.waitFor`; drive time-based logic with `vi.useFakeTimers` or `it.effect` and `TestClock.adjust`.",
      mixedClock:
        "Real-clock wait in a test. `setTimeout` runs on the wall clock while `it.effect` runs Effect time on the TestClock, so they never meet. Await the milestone, or use `Effect.yieldNow` to let forked fibers run.",
      undrivenTestClock:
        "Sleeps on the TestClock never fire without `TestClock.adjust`, so this test can only time out. Drive the TestClock, or await the milestone.",
      unexplainedSuppression:
        "Explain why this wait has nothing to await: `oxlint-disable-next-line anti-slop/no-real-time-wait-in-tests -- <reason>`.",
    },
  },
  create(context) {
    if (!testFilePattern.test(context.filename)) return {};
    const testClockDrives: Array<ESTree.CallExpression> = [];
    const timerPromiseLocals = new Set<string>();
    const timerPromiseNamespaces = new Set<string>();
    const fakeTimers: Array<FakeTimers> = [];
    const realTimerRestores: Array<ESTree.CallExpression> = [];
    const timerWaitsJudgedAtExit: Array<{ node: ESTree.CallExpression; timer: string; tester: Tester | null }> = [];
    const testClockWaitsJudgedAtExit: Array<{ node: ESTree.Node; messageId: "realClock" | "undrivenTestClock" }> = [];

    function timerPromiseCall(callee: ESTree.Node): string | null {
      if (callee.type === "Identifier") return timerPromiseLocals.has(callee.name) ? "setTimeout" : null;
      const call = memberCall(callee);
      if (call === null || !timerPromiseNamespaces.has(call[0]) || !timerFunctions.has(call[1])) return null;
      return call[1];
    }

    function globalTimer(callee: ESTree.Node): string | null {
      if (callee.type === "Identifier") return timerPromiseLocals.has(callee.name) ? null : callee.name;
      const call = memberCall(callee);
      return call !== null && timerGlobals.has(call[0]) ? call[1] : null;
    }

    function promiseWrappedTimer(node: ESTree.CallExpression): string | null {
      const timer = globalTimer(node.callee);
      if (timer === null || !timerFunctions.has(timer) || !isInsidePromiseExecutor(node)) return null;
      return timer;
    }

    function drivesTestClockFor(wait: ESTree.Node): boolean {
      const test = enclosingTestCall(wait);
      return testClockDrives.some((drive) => test === null || isAncestor(test, drive));
    }

    function checkSuppression(comment: Comment): void {
      if (suppression.test(comment.value) && !suppressionReason.test(comment.value)) {
        context.report({ node: comment, messageId: "unexplainedSuppression" });
      }
    }

    return {
      ImportDeclaration(node) {
        if (!timerPromiseModules.has(node.source.value)) return;
        for (const specifier of node.specifiers) {
          if (specifier.type === "ImportNamespaceSpecifier") {
            timerPromiseNamespaces.add(specifier.local.name);
          } else if (specifier.type === "ImportSpecifier") {
            const imported = propertyName(specifier.imported);
            if (imported !== null && timerFunctions.has(imported)) timerPromiseLocals.add(specifier.local.name);
          }
        }
      },
      CallExpression(node) {
        const callee = node.callee;
        if (callee.type === "Super" || callee.type === "V8IntrinsicExpression") return;

        const promiseTimer = timerPromiseCall(callee);
        if (promiseTimer !== null) {
          context.report({ node: callee, messageId: "realClock" });
          return;
        }
        const timer = promiseWrappedTimer(node);
        if (timer !== null) {
          timerWaitsJudgedAtExit.push({ node, timer, tester: enclosingTester(node) });
          return;
        }

        const call = memberCall(callee);
        if (call === null) return;
        const [object, property] = call;
        if (object === "vi" && property === "useFakeTimers") {
          fakeTimers.push({ call: node, fakes: fakedTimers(node) });
          return;
        }
        if (object === "vi" && property === "useRealTimers") {
          realTimerRestores.push(node);
          return;
        }
        if (object === "TestClock" && (property === "adjust" || property === "setTime")) {
          testClockDrives.push(node);
          return;
        }
        const isSleep = object === "Effect" && property === "sleep";
        const isWait =
          isSleep ||
          (object === "Effect" && property === "delay") ||
          (object === "Schedule" && scheduleWaits.has(property));
        if (!isWait) return;

        const tester = enclosingTester(node);
        if (tester === "live" || tester === "plain") {
          context.report({ node: callee, messageId: "realClock" });
        } else if (tester === null) {
          testClockWaitsJudgedAtExit.push({ node: callee, messageId: "realClock" });
        } else if (isSleep) {
          testClockWaitsJudgedAtExit.push({ node: callee, messageId: "undrivenTestClock" });
        }
      },
      "Program:exit"() {
        for (const wait of timerWaitsJudgedAtExit) {
          if (runsUnderFakeTimers(wait.node, wait.timer, fakeTimers, realTimerRestores)) continue;
          context.report({ node: wait.node.callee, messageId: wait.tester === "effect" ? "mixedClock" : "realClock" });
        }
        for (const wait of testClockWaitsJudgedAtExit) {
          if (!drivesTestClockFor(wait.node)) context.report(wait);
        }
        for (const comment of context.sourceCode.getAllComments()) checkSuppression(comment);
      },
    };
  },
});
