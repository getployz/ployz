import { defineRule } from "@oxlint/plugins";

import type { ESTree } from "@oxlint/plugins";

import { childNodes, propertyName, resolveVariable } from "../shared/ast.ts";

const testFilePattern = /\.test\.[cm]?[jt]sx?$/u;
const clockConversions = new Set(["getTime", "valueOf", "toISOString"]);
const rowWrites = new Set(["set", "values"]);
const conflictClauses = new Set(["onConflictDoUpdate", "onConflictDoNothing"]);
const conflictFilters = new Set(["set", "where", "setWhere", "targetWhere"]);
const checks = new Set(["where", "having"]);
const conflictChecks = new Set(["where", "setWhere", "targetWhere"]);
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

/**
 * SQL text that orders times: `<`, `>`, `<=`, `>=` or `between`, once `=>` (named arguments) and the JSON arrows are
 * taken out. A bare `=` does not count: in `update … set updated_at = now()` it writes the time.
 */
function compares(text: string): boolean {
  return /<|>|\bbetween\b/iu.test(text.replaceAll(/=>|->>?|#>>?|@>|<@/gu, " "));
}

/** The literal text of a sql template, a placeholder standing for each interpolated value. */
function templateText(node: ESTree.TaggedTemplateExpression): string {
  return node.quasi.quasis.map((quasi) => quasi.value.cooked ?? quasi.value.raw).join(" $ ");
}

function isSqlTemplate(node: ESTree.Node): node is ESTree.TaggedTemplateExpression {
  return node.type === "TaggedTemplateExpression" && node.tag.type === "Identifier" && node.tag.name === "sql";
}

/** Which clock a sql template reads that a check must not: `now()`, or a bare `clock_timestamp()`. */
function checkClock(text: string): "staleNow" | "volatileClock" | null {
  if (/\bnow\(\)/iu.test(text)) return "staleNow";
  if (/\bclock_timestamp\(\)/iu.test(text.replaceAll(/\(\s*select\s+clock_timestamp\(\)\s*\)/giu, " "))) return "volatileClock";
  return null;
}

/** Whether an expression can contain SQL a condition evaluates: not a function, an awaited query, or a method chain off anything but `sql`. */
function buildsSql(node: ESTree.Node): boolean {
  switch (node.type) {
    case "ArrowFunctionExpression":
    case "FunctionExpression":
    case "AwaitExpression":
    case "YieldExpression":
      return false;
    case "CallExpression":
      return node.callee.type !== "MemberExpression" || (node.callee.object.type === "Identifier" && node.callee.object.name === "sql");
    default:
      return true;
  }
}

/** The parts of a drizzle call that Postgres evaluates as a condition: `.where`, `.having` and an upsert's filters. */
function queryChecks(call: ESTree.CallExpression): Array<ESTree.Node> {
  const callee = call.callee;
  if (callee.type !== "MemberExpression" || callee.computed || call.arguments.length !== 1) return [];
  const method = propertyName(callee.property) ?? "";
  const [first] = call.arguments;
  if (first === undefined || first.type === "ArrowFunctionExpression" || first.type === "FunctionExpression") return [];
  if (checks.has(method)) return [first];
  if (conflictClauses.has(method) && first.type === "ObjectExpression") {
    return first.properties.flatMap((property) =>
      property.type === "Property" && conflictChecks.has(propertyName(property.key) ?? "") ? [property.value] : [],
    );
  }
  return [];
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
    docs: { description: "Disallow the web instance's clock in Postgres, and a check on Postgres's transaction-start clock." },
    messages: {
      staleNow: "A check reads the time it runs at; now() is the transaction start, stale after any lock wait. Use checkedNow().",
      volatileClock:
        "clock_timestamp() is volatile, so Postgres reads it per row and no index can serve this check. Use checkedNow(), which reads it once per statement.",
      webClock:
        "Postgres stamps and checks stored times on its own clock; a web instance's clock drifts from it. Write sql`now()` (plus an interval), and check against checkedNow().",
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

    /** A `const` anywhere stands for its initializer: a check against `cutoff` reads what `cutoff` was built from. */
    function constValue(node: ESTree.Node): ESTree.Node {
      if (node.type !== "Identifier") return node;
      const definition = resolveVariable(sourceCode, node as ESTree.IdentifierReference)?.defs[0];
      const declarator = definition?.node;
      const declaration = definition?.parent;
      if (declarator?.type !== "VariableDeclarator" || declarator.init === null || declaration?.type !== "VariableDeclaration"
        || declaration.kind !== "const") return node;
      return unwrapped(declarator.init);
    }

    /**
     * Every sql template a condition reads, through consts and nested templates. A query's result, a method chain and
     * a nested function are not SQL the condition contains: `inArray(id, ended.map(...))` reads rows, not `ended`'s set.
     */
    function checkClocks(wrapped: ESTree.Node, seen: Set<ESTree.Node>) {
      const node = constValue(unwrapped(wrapped));
      if (seen.has(node)) return;
      seen.add(node);
      if (isSqlTemplate(node)) {
        const clock = checkClock(templateText(node));
        if (clock !== null && !reported.has(node)) {
          reported.add(node);
          context.report({ node, messageId: clock });
        }
        for (const expression of node.quasi.expressions) checkClocks(expression, seen);
        return;
      }
      if (!buildsSql(node)) return;
      for (const child of childNodes(node, visitorKeys)) {
        if (node.type === "MemberExpression" && child === node.property && !node.computed) continue;
        if (node.type === "Property" && child === node.key && !node.computed) continue;
        checkClocks(child, seen);
      }
    }

    return {
      TaggedTemplateExpression(node) {
        if (isSqlTemplate(node) && compares(templateText(node))) checkClocks(node, new Set());
      },
      CallExpression(node) {
        for (const condition of queryChecks(node)) checkClocks(condition, new Set());
        if (isComparison(node.callee)) {
          for (const argument of node.arguments) {
            report(argument);
            checkClocks(argument, new Set());
          }
          return;
        }
        const [first] = node.arguments;
        if (first === undefined) return;
        for (const value of queryValues(node, resolveConst(first))) report(resolveConst(value));
      },
    };
  },
});
