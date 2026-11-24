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
const clockHelpers = new Set(["checkedNow", "sweepCutoff"]);
const lockingRead = /\bfor\s+(?:no\s+key\s+update|update|key\s+share|share)\b/iu;
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
 * SQL text that compares: `<`, `>`, `!=` or `between`, once `=>` (named arguments) and the JSON arrows are taken out,
 * and `=` unless the text is a value a write assigns, as in `.set({ endedAt: sql\`case when … = … then now() end\` })`.
 */
function compares(text: string, written: boolean): boolean {
  const operators = text.replaceAll(/=>|->>?|#>>?|@>|<@/gu, " ");
  return /<|>|!=|\bbetween\b/iu.test(operators) || (!written && operators.includes("="));
}

function isSqlTemplate(node: ESTree.Node): node is ESTree.TaggedTemplateExpression {
  return node.type === "TaggedTemplateExpression" && node.tag.type === "Identifier" && node.tag.name === "sql";
}

function isSqlRaw(node: ESTree.Node): node is ESTree.CallExpression {
  return node.type === "CallExpression" && isMember(node.callee, "sql", "raw") && node.arguments.length === 1;
}

/** The literal text of a template, a placeholder standing for each interpolated value. */
function quasiText(node: ESTree.TemplateLiteral): string {
  return node.quasis.map((quasi) => quasi.value.cooked ?? quasi.value.raw).join(" $ ");
}

/** Which clock SQL text reads that a check must not name directly: the transaction's start, or `clock_timestamp()`. */
function checkClock(text: string): "staleNow" | "rawClock" | null {
  if (/\b(?:now|transaction_timestamp|statement_timestamp)\s*\(\s*\)|\bcurrent_timestamp\b/iu.test(text)) return "staleNow";
  if (/\bclock_timestamp\s*\(\s*\)/iu.test(text)) return "rawClock";
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

function methodOf(call: ESTree.CallExpression): string | null {
  return call.callee.type === "MemberExpression" && !call.callee.computed ? propertyName(call.callee.property) : null;
}

/**
 * The calls of a method chain, outermost first, when `call` ends it: `drizzle.select().from(t).for("update")` gives the
 * `for`, `from` and `select` calls, and `select.for(...)` continues into the chain the `const select` holds. Null for a
 * call further down a chain, which its outermost call stands for.
 */
function chainCalls(call: ESTree.CallExpression, resolve: (node: ESTree.Node) => ESTree.Node): Array<ESTree.CallExpression> | null {
  const parent = call.parent;
  if (parent?.type === "MemberExpression" && parent.object === call && parent.parent?.type === "CallExpression"
    && parent.parent.callee === parent) return null;
  const calls: Array<ESTree.CallExpression> = [];
  const seen = new Set<ESTree.Node>();
  for (let current: ESTree.Node = call; current.type === "CallExpression" && methodOf(current) !== null && !seen.has(current);) {
    seen.add(current);
    calls.push(current);
    current = resolve(unwrapped((current.callee as ESTree.MemberExpression).object));
  }
  return calls;
}

/**
 * What a statement does with the rows its time check lets through. A locking read checks them before it waits for the
 * lock; an UPDATE, an upsert's update and a DELETE that returns rows act on them. A plain read and a DELETE that
 * returns nothing, a prune, only see or drop them, so a cutoff from when the transaction began costs a run's delay.
 */
function rowUse(calls: ReadonlyArray<ESTree.CallExpression>): "locks" | "writes" | null {
  const methods = calls.map(methodOf);
  if (methods.includes("for")) return "locks";
  const first = methods.at(-1);
  if (first === "update" || methods.includes("onConflictDoUpdate")) return "writes";
  if (first === "delete" && methods.includes("returning")) return "writes";
  return null;
}

/** The same for raw SQL text. */
function textRowUse(text: string): "locks" | "writes" | null {
  if (lockingRead.test(text)) return "locks";
  if (/\bupdate\b/iu.test(text) || (/\bdelete\b/iu.test(text) && /\breturning\b/iu.test(text))) return "writes";
  return null;
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
    docs: { description: "Disallow the web instance's clock in Postgres, and a check that reads Postgres's clock other than through checkedNow() or sweepCutoff()." },
    messages: {
      staleNow:
        "now() and current_timestamp are when the transaction began, and a recheck after a lock wait reuses it, so a deadline that passed during the wait still passes. "
        + "Use checkedNow(). Use sweepCutoff() only in a sweep that needs an index on the time column and can act on a just-expired row one run late.",
      rawClock:
        "A check names its clock through a helper. Use checkedNow() for clock_timestamp(). "
        + "Use sweepCutoff() only in a sweep that needs an index on the time column and can act on a just-expired row one run late.",
      lockedCheck:
        "A locking read evaluates this before it waits for the lock. Lock the row by id first, then check checkedNow() in the next statement.",
      sweepActs:
        "sweepCutoff() is when the transaction began, so a row that expired since still passes, and this statement locks or changes the rows it lets through. "
        + "Only a plain range read, or a prune that returns nothing, may use it. Check against checkedNow(), after the lock when the read locks.",
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

    /** The `const` an identifier names, with its initializer. */
    function constOf(node: ESTree.Node) {
      if (node.type !== "Identifier") return null;
      const variable = resolveVariable(sourceCode, node as ESTree.IdentifierReference);
      const definition = variable?.defs[0];
      const declarator = definition?.node;
      const declaration = definition?.parent;
      if (declarator?.type !== "VariableDeclarator" || declarator.init === null || declaration?.type !== "VariableDeclaration"
        || declaration.kind !== "const") return null;
      return { variable, init: declarator.init };
    }

    /** A `const` anywhere stands for its initializer, through any chain of aliases: `limit` reads what `cutoff` was built from. */
    function constValue(node: ESTree.Node): ESTree.Node {
      let current = node;
      const seen = new Set<ESTree.Node>();
      for (let found = constOf(current); found !== null && !seen.has(current); found = constOf(current)) {
        seen.add(current);
        current = unwrapped(found.init);
      }
      return current;
    }

    /** The SQL text of a sql template, or of `sql.raw` given a string or a `const` holding one. */
    function sqlText(node: ESTree.Node): string | null {
      if (isSqlTemplate(node)) return quasiText(node.quasi);
      if (!isSqlRaw(node)) return null;
      const [argument] = node.arguments;
      const text = argument === undefined ? null : constValue(unwrapped(argument));
      if (text?.type === "Literal" && typeof text.value === "string") return text.value;
      if (text?.type === "TemplateLiteral") return quasiText(text);
      return null;
    }

    /**
     * Whether an expression is only ever a value a row write assigns: in the object `.set` or `.values` takes, an
     * upsert's `set`, through spreads, arrays and `const`s. There an `=` assigns; everywhere else it compares.
     */
    function isWritten(node: ESTree.Node, seen = new Set<ESTree.Node>()): boolean {
      if (seen.has(node)) return false;
      seen.add(node);
      let current = node;
      let parent = current.parent;
      while (parent !== null && parent !== undefined && (parent.type === "TSAsExpression" || parent.type === "TSSatisfiesExpression"
        || parent.type === "TSNonNullExpression" || parent.type === "ParenthesizedExpression")) {
        current = parent;
        parent = parent.parent;
      }
      if (parent === null || parent === undefined) return false;
      switch (parent.type) {
        case "Property": {
          const object = parent.parent;
          if (parent.value !== current || object?.type !== "ObjectExpression") return false;
          const call = object.parent;
          if (propertyName(parent.key) === "set" && call?.type === "CallExpression" && call.arguments[0] === object
            && call.callee.type === "MemberExpression" && propertyName(call.callee.property) === "onConflictDoUpdate") return true;
          return isWritten(object, seen);
        }
        case "ArrayExpression":
        case "SpreadElement":
          return isWritten(parent, seen);
        case "CallExpression": {
          const callee = parent.callee;
          return parent.arguments.length === 1 && parent.arguments[0] === current && callee.type === "MemberExpression"
            && !callee.computed && rowWrites.has(propertyName(callee.property) ?? "");
        }
        case "VariableDeclarator": {
          if (parent.init !== current || parent.id.type !== "Identifier") return false;
          const found = constOf(parent.id);
          const reads = found?.variable?.references.filter((reference) => reference.identifier !== parent.id) ?? [];
          return reads.length > 0 && reads.every((reference) => isWritten(reference.identifier, seen));
        }
        default:
          return false;
      }
    }

    /**
     * Every sql template a condition reads, through consts and nested templates. A query's result, a method chain and
     * a nested function are not SQL the condition contains: `inArray(id, ended.map(...))` reads rows, not `ended`'s set.
     */
    function checkClocks(wrapped: ESTree.Node, seen: Set<ESTree.Node>) {
      const node = constValue(unwrapped(wrapped));
      if (seen.has(node)) return;
      seen.add(node);
      const text = sqlText(node);
      if (text !== null) {
        const clock = checkClock(text);
        if (clock !== null && !reported.has(node)) {
          reported.add(node);
          context.report({ node, messageId: clock });
        }
        if (isSqlTemplate(node)) for (const expression of node.quasi.expressions) checkClocks(expression, seen);
        return;
      }
      if (!buildsSql(node)) return;
      for (const child of childNodes(node, visitorKeys)) {
        if (node.type === "MemberExpression" && child === node.property && !node.computed) continue;
        if (node.type === "Property" && child === node.key && !node.computed) continue;
        checkClocks(child, seen);
      }
    }

    /** Every checkedNow() and sweepCutoff() a statement's part reads, through consts, sql templates and subqueries. */
    function helperCalls(wrapped: ESTree.Node, seen: Set<ESTree.Node>): Array<ESTree.CallExpression> {
      const node = constValue(unwrapped(wrapped));
      if (seen.has(node)) return [];
      seen.add(node);
      if (node.type === "CallExpression" && node.callee.type === "Identifier" && clockHelpers.has(node.callee.name)) return [node];
      if (node.type === "ArrowFunctionExpression" || node.type === "FunctionExpression" || node.type === "AwaitExpression"
        || node.type === "YieldExpression") return [];
      return childNodes(node, visitorKeys).flatMap((child) => {
        if (node.type === "MemberExpression" && child === node.property && !node.computed) return [];
        if (node.type === "Property" && child === node.key && !node.computed) return [];
        return helperCalls(child, seen);
      });
    }

    function checkRowUse(use: "locks" | "writes" | null, parts: ReadonlyArray<ESTree.Node>) {
      if (use === null) return;
      const seen = new Set<ESTree.Node>();
      for (const call of parts.flatMap((part) => helperCalls(part, seen))) {
        const sweep = (call.callee as ESTree.IdentifierReference).name === "sweepCutoff";
        if ((!sweep && use !== "locks") || reported.has(call)) continue;
        reported.add(call);
        context.report({ node: call, messageId: sweep ? "sweepActs" : "lockedCheck" });
      }
    }

    return {
      TaggedTemplateExpression(node) {
        if (!isSqlTemplate(node)) return;
        const text = quasiText(node.quasi);
        if (compares(text, isWritten(node))) checkClocks(node, new Set());
        checkRowUse(textRowUse(text), node.quasi.expressions);
      },
      CallExpression(node) {
        const calls = chainCalls(node, constValue);
        if (calls !== null) checkRowUse(rowUse(calls), calls.flatMap((call) => call.arguments));
        const raw = isSqlRaw(node) ? sqlText(node) : null;
        if (raw !== null && compares(raw, isWritten(node))) checkClocks(node, new Set());
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
