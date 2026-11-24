import { defineRule } from "@oxlint/plugins";

import type { ESTree } from "@oxlint/plugins";

import { childNodes, propertyName, resolveVariable } from "../shared/ast.ts";

const testFilePattern = /\.test\.[cm]?[jt]sx?$/u;
const clockConversions = new Set(["getTime", "valueOf", "toISOString"]);
const rowWrites = new Set(["set", "values"]);
const conflictClauses = new Set(["onConflictDoUpdate", "onConflictDoNothing"]);
const conflictFilters = new Set(["set", "where", "setWhere", "targetWhere"]);
const comparisons = new Set(["eq", "ne", "gt", "gte", "lt", "lte", "between", "notBetween", "inArray", "notInArray"]);

function isMember(node: ESTree.Node, object: string, property: string): boolean {
  return (
    node.type === "MemberExpression" &&
    !node.computed &&
    node.object.type === "Identifier" &&
    node.object.name === object &&
    propertyName(node.property) === property
  );
}

/** The expression under `as`, `satisfies`, `!` and parentheses, which leave the value a query gets as it is. */
function unwrapped(node: ESTree.Node): ESTree.Node {
  let current = node;
  while (current.type === "TSAsExpression" || current.type === "TSSatisfiesExpression" || current.type === "TSNonNullExpression"
    || current.type === "ParenthesizedExpression") current = current.expression;
  return current;
}

function enclosingFunction(node: ESTree.Node): ESTree.Node | null {
  for (let current = node.parent; current !== null; current = current.parent) {
    if (current.type === "ArrowFunctionExpression" || current.type === "FunctionExpression" || current.type === "FunctionDeclaration") {
      return current;
    }
  }
  return null;
}

/**
 * The parts of a drizzle call that reach Postgres as values: `.set({...})`, `.values({...})`, `.where(...)` on a
 * query, and the `set` and filters of an upsert. Two-argument `Map#set` and callback `.where` (TanStack DB) are not.
 * `first` is the call's argument, or the initializer of the `const` it names.
 */
function queryValues(call: ESTree.CallExpression, first: ESTree.Node): Array<ESTree.Node> {
  const callee = call.callee;
  if (callee.type !== "MemberExpression" || callee.computed) return [];
  const method = propertyName(callee.property);
  if (method === null || call.arguments.length !== 1) return [];
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
        "Readers compare this against Postgres's now(); a web instance's clock drifts from it. Use sql`now()` (plus an interval) instead, or sql`clock_timestamp()` for a check made after a lock: now() is when the transaction began, before any wait.",
    },
  },
  create(context) {
    if (testFilePattern.test(context.filename)) return {};
    const { sourceCode } = context;
    const { visitorKeys } = sourceCode;
    const clockVariables = new Map<ESTree.Node, boolean>();
    const reported = new Set<ESTree.Node>();

    /** `gt`, `lt` and the other comparisons imported from drizzle-orm, under any local name. */
    function isComparison(callee: ESTree.Node): boolean {
      if (callee.type !== "Identifier") return false;
      const definition = resolveVariable(sourceCode, callee)?.defs[0];
      if (definition?.type !== "ImportBinding" || definition.node.type !== "ImportSpecifier") return false;
      const declaration = definition.parent;
      return declaration?.type === "ImportDeclaration" && declaration.source.value === "drizzle-orm"
        && comparisons.has(propertyName(definition.node.imported) ?? "");
    }

    /** A `const` declared in the same function stands for what it was built as: `.set(set)` checks `set`'s object. */
    function resolveConst(wrapped: ESTree.Node): ESTree.Node {
      const node = unwrapped(wrapped);
      if (node.type !== "Identifier") return node;
      const definition = resolveVariable(sourceCode, node as ESTree.IdentifierReference)?.defs[0];
      const declarator = definition?.node;
      if (declarator?.type !== "VariableDeclarator" || declarator.init === null) return node;
      const declaration = definition?.parent;
      if (declaration?.type !== "VariableDeclaration" || declaration.kind !== "const") return node;
      return enclosingFunction(declarator) === enclosingFunction(node) ? unwrapped(declarator.init) : node;
    }

    function report(value: ESTree.Node) {
      for (const read of clockReads(value)) {
        if (reported.has(read)) continue;
        reported.add(read);
        context.report({ node: read, messageId: "webClock" });
      }
    }

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
        case "TSSatisfiesExpression":
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
      if (node.type === "SpreadElement") return clockReads(resolveConst(node.argument));
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
        if (isComparison(node.callee)) {
          for (const argument of node.arguments) report(argument);
          return;
        }
        const [first] = node.arguments;
        if (first === undefined) return;
        for (const value of queryValues(node, resolveConst(first))) report(resolveConst(value));
      },
    };
  },
});
