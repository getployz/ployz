import { defineRule } from "@oxlint/plugins";

import type { ESTree } from "@oxlint/plugins";

import { childNodes, propertyName, resolveVariable } from "../shared/ast.ts";

const testFilePattern = /\.test\.[cm]?[jt]sx?$/u;
const clockConversions = new Set(["getTime", "valueOf", "toISOString"]);
const rowWrites = new Set(["set", "values"]);
const conflictClauses = new Set(["onConflictDoUpdate", "onConflictDoNothing"]);
const conflictFilters = new Set(["set", "where", "setWhere", "targetWhere"]);

function isMember(node: ESTree.Node, object: string, property: string): boolean {
  return (
    node.type === "MemberExpression" &&
    !node.computed &&
    node.object.type === "Identifier" &&
    node.object.name === object &&
    propertyName(node.property) === property
  );
}

/**
 * The parts of a drizzle call that reach Postgres as values: `.set({...})`, `.values({...})`, `.where(...)` on a
 * query, and the `set` and filters of an upsert. Two-argument `Map#set` and callback `.where` (TanStack DB) are not.
 */
function queryValues(call: ESTree.CallExpression): Array<ESTree.Node> {
  const callee = call.callee;
  if (callee.type !== "MemberExpression" || callee.computed) return [];
  const method = propertyName(callee.property);
  const [first] = call.arguments;
  if (method === null || first === undefined || call.arguments.length !== 1) return [];
  if (rowWrites.has(method)) return first.type === "ObjectExpression" || first.type === "ArrayExpression" ? [first] : [];
  if (method === "where") return first.type === "ArrowFunctionExpression" || first.type === "FunctionExpression" ? [] : [first];
  if (conflictClauses.has(method) && first.type === "ObjectExpression") {
    return first.properties.flatMap((property) =>
      property.type === "Property" && conflictFilters.has(propertyName(property.key) ?? "") ? [property.value] : [],
    );
  }
  return [];
}

export const postgresClockInQueriesRule = defineRule({
  meta: {
    type: "problem",
    docs: { description: "Disallow the web instance's clock as a value written to or compared in Postgres." },
    messages: {
      webClock:
        "Readers compare this against Postgres's now(); a web instance's clock drifts from it. Use sql`now()` (plus an interval) instead.",
    },
  },
  create(context) {
    if (testFilePattern.test(context.filename)) return {};
    const { sourceCode } = context;
    const { visitorKeys } = sourceCode;
    const clockVariables = new Map<ESTree.Node, boolean>();
    const reported = new Set<ESTree.Node>();

    /** `const now = new Date()` and the like: a variable whose initial value is a reading of the clock. */
    function namesClock(identifier: ESTree.IdentifierReference): boolean {
      const definition = resolveVariable(sourceCode, identifier)?.defs[0];
      if (definition?.node.type !== "VariableDeclarator" || definition.node.init === null) return false;
      const declarator = definition.node;
      const known = clockVariables.get(declarator);
      if (known !== undefined) return known;
      clockVariables.set(declarator, false);
      const reads = declarator.init !== null && isClockValue(declarator.init);
      clockVariables.set(declarator, reads);
      return reads;
    }

    /** `Date.now()`, `new Date()`, Effect's `Clock.currentTimeMillis`, a name for one, or arithmetic on one. */
    function isClockValue(node: ESTree.Node): boolean {
      switch (node.type) {
        case "CallExpression":
          if (isMember(node.callee, "Date", "now")) return true;
          return node.callee.type === "MemberExpression" && clockConversions.has(propertyName(node.callee.property) ?? "")
            && isClockValue(node.callee.object);
        case "NewExpression":
          return node.callee.type === "Identifier" && node.callee.name === "Date"
            && (node.arguments.length === 0 || node.arguments.some(isClockValue));
        case "MemberExpression":
          return isMember(node, "Clock", "currentTimeMillis");
        case "Identifier":
          return namesClock(node as ESTree.IdentifierReference);
        case "BinaryExpression":
          return isClockValue(node.left) || isClockValue(node.right);
        case "AwaitExpression":
        case "YieldExpression":
          return node.argument !== null && node.argument !== undefined && isClockValue(node.argument);
        case "TSAsExpression":
        case "TSNonNullExpression":
        case "ParenthesizedExpression":
          return isClockValue(node.expression);
        default:
          return false;
      }
    }

    /**
     * Every outermost clock value in an expression. An object or array literal passed to a call is a payload, such as
     * a jsonb record the app reads back itself, not a value Postgres compares; nested functions run elsewhere.
     */
    function clockReads(node: ESTree.Node): Array<ESTree.Node> {
      if (isClockValue(node)) return [node];
      if (node.type === "ArrowFunctionExpression" || node.type === "FunctionExpression") return [];
      return childNodes(node, visitorKeys).flatMap((child) => {
        if (node.type === "MemberExpression" && child === node.property && !node.computed) return [];
        if (node.type === "Property" && child === node.key && !node.computed) return [];
        const payload = child.type === "ObjectExpression" || child.type === "ArrayExpression";
        if (payload && (node.type === "CallExpression" || node.type === "NewExpression")) return [];
        return clockReads(child);
      });
    }

    return {
      CallExpression(node) {
        for (const value of queryValues(node)) {
          for (const read of clockReads(value)) {
            if (reported.has(read)) continue;
            reported.add(read);
            context.report({ node: read, messageId: "webClock" });
          }
        }
      },
    };
  },
});
