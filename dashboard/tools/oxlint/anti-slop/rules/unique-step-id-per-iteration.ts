import { defineRule } from "@oxlint/plugins";

import type { ESTree } from "@oxlint/plugins";

import { childNodes, propertyName, readIdentifiers, resolveVariable, within } from "../shared/ast.ts";

type VisitorKeys = Readonly<Record<string, readonly string[]>>;

const memoizedSteps = new Set(["run", "sleep", "sleepUntil", "invoke", "waitForEvent", "sendEvent"]);
const iterators = new Set(["map", "flatMap", "forEach"]);

/** `step.run(...)`, or `steps.step.run(...)` on a tools object. */
function isStepCall(callee: ESTree.Node): boolean {
  if (callee.type !== "MemberExpression" || callee.computed) return false;
  const method = propertyName(callee.property);
  if (method === null || !memoizedSteps.has(method)) return false;
  const tools = callee.object;
  if (tools.type === "Identifier") return tools.name === "step";
  return tools.type === "MemberExpression" && !tools.computed && propertyName(tools.property) === "step";
}

/** The innermost loop a node runs in: a loop statement, or the callback of `.map`, `.flatMap` or `.forEach`. */
function innermostLoop(node: ESTree.Node): ESTree.Node | null {
  for (let current = node.parent; current !== null; current = current.parent) {
    switch (current.type) {
      case "ForStatement":
      case "ForInStatement":
      case "ForOfStatement":
      case "WhileStatement":
      case "DoWhileStatement":
        return current;
      case "ArrowFunctionExpression":
      case "FunctionExpression": {
        const call = current.parent;
        if (
          call?.type === "CallExpression" &&
          call.arguments.some((argument) => argument === current) &&
          call.callee.type === "MemberExpression" &&
          iterators.has(propertyName(call.callee.property) ?? "")
        ) {
          return current;
        }
      }
    }
  }
  return null;
}

function hasContinue(node: ESTree.Node, visitorKeys: VisitorKeys): boolean {
  if (node.type === "ContinueStatement") return true;
  if (isFunctionNode(node)) return false;
  return childNodes(node, visitorKeys).some((child) => hasContinue(child, visitorKeys));
}

function isFunctionNode(node: ESTree.Node): boolean {
  return node.type === "ArrowFunctionExpression" || node.type === "FunctionExpression" || node.type === "FunctionDeclaration";
}

/** A statement after which the iteration never goes on: it returns, throws or breaks on every path. */
function alwaysLeaves(statement: ESTree.Node, breakLeaves: boolean): boolean {
  switch (statement.type) {
    case "ReturnStatement":
    case "ThrowStatement":
      return true;
    case "BreakStatement":
      return breakLeaves && statement.label === null;
    case "BlockStatement":
      return statement.body.some((child) => alwaysLeaves(child, breakLeaves));
    case "IfStatement":
      return statement.alternate !== null && alwaysLeaves(statement.consequent, breakLeaves) && alwaysLeaves(statement.alternate, breakLeaves);
    case "TryStatement":
      return (statement.finalizer !== null && alwaysLeaves(statement.finalizer, breakLeaves))
        || (alwaysLeaves(statement.block, breakLeaves) && (statement.handler === null || alwaysLeaves(statement.handler.body, breakLeaves)));
    default:
      return false;
  }
}

/**
 * A step the iteration cannot outlive runs once per run, however many times the loop turned before it: it is returned,
 * or a later statement on its way out of the loop returns, throws or breaks before anything can continue. Only a loop
 * statement can be left this way: a return in an iterator callback ends one call, and the next element runs the step.
 */
function endsIteration(call: ESTree.Node, loop: ESTree.Node, visitorKeys: VisitorKeys): boolean {
  if (isFunctionNode(loop)) return false;
  let breakLeaves = true;
  let child: ESTree.Node = call;
  for (let current = call.parent; current !== null && current !== loop; current = current.parent) {
    if (current.type === "ReturnStatement" || current.type === "ThrowStatement") return true;
    if (current.type === "SwitchStatement") breakLeaves = false;
    if (isFunctionNode(current)) return false;
    if (current.type === "BlockStatement") {
      const after = current.body.slice(current.body.indexOf(child as ESTree.Statement) + 1);
      for (const statement of after) {
        if (alwaysLeaves(statement, breakLeaves)) return true;
        if (hasContinue(statement, visitorKeys)) return false;
      }
    }
    child = current;
  }
  return false;
}

export const uniqueStepIdPerIterationRule = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Require a step inside a loop to take an id built from the loop's key." },
    messages: {
      sharedId:
        "Inngest memoizes a step by its id, so every iteration replays the first: build the id from the loop key.",
    },
  },
  create(context) {
    const { sourceCode } = context;

    /** Declared in the loop (its head, body or callback parameters), or reassigned on each pass. */
    function changesPerIteration(loop: ESTree.Node, identifier: ESTree.IdentifierReference): boolean {
      const variable = resolveVariable(sourceCode, identifier);
      if (variable === null) return false;
      if (variable.defs.some((definition) => within(loop, definition.name))) return true;
      return variable.references.some((reference) => reference.isWrite() && !reference.init && within(loop, reference.identifier));
    }

    return {
      CallExpression(node) {
        if (!isStepCall(node.callee)) return;
        const id = node.arguments[0];
        if (id === undefined) return;
        const fixed = (id.type === "Literal" && typeof id.value === "string") || id.type === "TemplateLiteral";
        if (!fixed) return;
        const loop = innermostLoop(node);
        if (loop === null || endsIteration(node, loop, sourceCode.visitorKeys)) return;
        const keyed = readIdentifiers(id, sourceCode.visitorKeys).some((identifier) => changesPerIteration(loop, identifier));
        if (!keyed) context.report({ node: id, messageId: "sharedId" });
      },
    };
  },
});
