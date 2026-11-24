/**
 * Lists every timestamp column a drizzle write stamps with Postgres's `now()`, and every reader of those columns in
 * src/, classified by which clock decides. A column Postgres stamps must be compared against Postgres's clock: a
 * reader that holds it against `Date.now()` mixes two clocks.
 *
 *   node scripts/clock-readers.ts [--base <rev>] [--all]
 *
 * `--base` keeps only columns whose `now()` write the diff against <rev> added (default: every `now()` write).
 * `--all` prints display-only readers too. Exits 1 while a JS-clock decider or writer has no reason in KEPT.
 */
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import { parseSync, visitorKeys } from "oxc-parser";

/** The ESTree fields this script reads; oxc-parser fills whichever a node's type has. */
type Node = {
  type: string; start: number; end: number; parent?: Node | null;
  name?: string; value?: unknown; operator?: string; computed?: boolean;
  callee?: Node; object?: Node; property?: Node; arguments?: Node[]; tag?: Node; quasi?: Node; quasis?: Node[]; expressions?: Node[];
  id?: Node; init?: Node | null; key?: Node; params?: Node[]; declarations?: Node[]; declaration?: Node | null; body?: Node | Node[];
  argument?: Node | null; left?: Node; right?: Node; expression?: Node; imported?: Node; local?: Node; properties?: Node[];
};
type Column = { table: string; field: string; snake: string };
type Reader = { column: string; at: string; via: "drizzle" | "row" | "sql" | "write"; verdict: Verdict; note: string };
type Verdict = "sql-clock" | "row-vs-row" | "display" | "js-clock" | "js-write";

const root = join(import.meta.dirname, "..");
const src = join(root, "src");
const args = process.argv.slice(2);
const base = args.includes("--base") ? args[args.indexOf("--base") + 1] : undefined;
const showAll = args.includes("--all");

const sourceFiles = (dir: string): string[] => readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
  const path = join(dir, entry.name);
  if (entry.isDirectory()) return sourceFiles(path);
  return /\.tsx?$/u.test(entry.name) && !/\.test\.tsx?$/u.test(entry.name) ? [path] : [];
});

const isNode = (value: unknown): value is Node =>
  typeof value === "object" && value !== null && typeof (value as { type?: unknown }).type === "string";

function children(node: Node): Node[] {
  const keys = (visitorKeys as Record<string, readonly string[]>)[node.type] ?? [];
  return keys.flatMap((key) => {
    const value = (node as unknown as Record<string, unknown>)[key];
    if (Array.isArray(value)) return value.filter(isNode);
    return isNode(value) ? [value] : [];
  });
}

function walk(node: Node, visit: (node: Node) => void, parent: Node | null = null) {
  node.parent = parent;
  visit(node);
  for (const child of children(node)) walk(child, visit, node);
}

type File = { path: string; text: string; nodes: Node[]; lineOf: (offset: number) => number };
const files: File[] = sourceFiles(src).map((path) => {
  const text = readFileSync(path, "utf8");
  const { program } = parseSync(path, text);
  const nodes: Node[] = [];
  walk(program as unknown as Node, (node) => nodes.push(node));
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === "\n") starts.push(i + 1);
  const lineOf = (offset: number) => {
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (starts[mid]! <= offset) lo = mid;
      else hi = mid - 1;
    }
    return lo + 1;
  };
  return { path, text, nodes, lineOf };
});

const name = (node: Node | undefined | null): string | null => {
  if (!node) return null;
  if (node.type === "Identifier") return node.name as string;
  if (node.type === "Literal" && typeof node.value === "string") return node.value;
  return null;
};
const calleeMethod = (call: Node) => {
  const callee = call.callee as Node;
  return callee.type === "MemberExpression" && !callee.computed ? name(callee.property as Node) : null;
};
const sqlText = (template: Node) => (template.quasis as Node[]).map((quasi) => (quasi.value as { raw: string }).raw).join("${}");
const isSqlTemplate = (node: Node) => node.type === "TaggedTemplateExpression" && name(node.tag as Node) === "sql";
const postgresClock = /\b(now|clock_timestamp)\(\)/u;

/** A top-level `const` or function anywhere in src, by name: what a spread or shorthand column refers to. */
function topLevel(named: string): Node | null {
  for (const file of files) {
    for (const node of file.nodes) {
      if (node.type === "VariableDeclarator" && name(node.id as Node) === named && node.parent?.parent?.type !== "BlockStatement") return (node.init as Node | null) ?? null;
      if (node.type === "FunctionDeclaration" && name(node.id as Node) === named) {
        const body = (node.body as Node).body as Node[];
        const returned = body.find((statement) => statement.type === "ReturnStatement");
        return (returned?.argument as Node | null) ?? null;
      }
    }
  }
  return null;
}

/** The SQL name a column builder gives: `timestamp("snake", ...)` under any chain of `.notNull()` and the like. */
function snakeOf(value: Node | null | undefined, depth = 0): string | null {
  while (value?.type === "CallExpression" && (value.callee as Node).type === "MemberExpression") value = (value.callee as Node).object as Node;
  if (value?.type === "CallExpression") return name((value.arguments as Node[])[0]);
  if (value?.type === "Identifier" && depth < 3) return snakeOf(topLevel(value.name as string), depth + 1);
  return null;
}

/** A table's columns, through spreads (`...owner`, `...attemptColumns()`) and shorthand (`updatedAt,`). */
function columnsOf(object: Node | null, fields: Map<string, string>, depth = 0) {
  if (object?.type === "Identifier" && depth < 3) return columnsOf(topLevel(object.name as string), fields, depth + 1);
  if (object?.type === "CallExpression" && depth < 3) return columnsOf(topLevel(name(object.callee as Node) ?? ""), fields, depth + 1);
  if (object?.type !== "ObjectExpression") return;
  for (const property of object.properties as Node[]) {
    if (property.type === "SpreadElement") columnsOf(property.argument as Node, fields, depth + 1);
    const field = name(property.key as Node);
    const snake = snakeOf(property.value as Node);
    if (field && snake) fields.set(field, snake);
  }
}

// Tables: `export const x = pgTable("x", { field: timestamp("snake", ...) })`, keyed by the const's name.
const tables = new Map<string, Map<string, string>>();
for (const file of files) {
  for (const node of file.nodes) {
    if (node.type !== "VariableDeclarator" || (node.init as Node | null)?.type !== "CallExpression") continue;
    const init = node.init as Node;
    if (name(init.callee as Node) !== "pgTable") continue;
    const fields = new Map<string, string>();
    columnsOf((init.arguments as Node[])[1] ?? null, fields);
    tables.set(name(node.id as Node)!, fields);
  }
}

/** The local names a file gives each table it imports or declares. */
function tableNames(file: File): Map<string, string> {
  const local = new Map<string, string>();
  for (const table of tables.keys()) local.set(table, table);
  for (const node of file.nodes) {
    if (node.type !== "ImportSpecifier") continue;
    const imported = name(node.imported as Node);
    if (imported && tables.has(imported)) local.set(name(node.local as Node)!, imported);
  }
  return local;
}

/** The `const` or parameter an identifier names, from its scope outward. */
function declaration(identifier: Node): { init: Node | null } | { fn: string; index: number } | null {
  const named = identifier.name as string;
  for (let scope = identifier.parent; scope; scope = scope.parent) {
    const declarators = children(scope).flatMap((child) => {
      if (child.type === "VariableDeclaration") return child.declarations as Node[];
      if (child.type === "ExportNamedDeclaration" && (child.declaration as Node | null)?.type === "VariableDeclaration") return (child.declaration as Node).declarations as Node[];
      return [];
    });
    const declared = declarators.find((declarator) => name(declarator.id as Node) === named);
    if (declared) return { init: (declared.init as Node | null) ?? null };
    const index = ((scope.params as Node[] | undefined) ?? []).findIndex((param) => name((param.type === "AssignmentPattern" ? param.left : param) as Node) === named);
    if (index >= 0) return { fn: functionName(scope) ?? "?", index };
  }
  return null;
}

/** What a function is called by: its declared name, the `const` it is assigned to, or the property it is the value of. */
function functionName(fn: Node): string | null {
  if (fn.type === "FunctionDeclaration") return name(fn.id as Node);
  // `Effect.fn("name")(function* () {})` and `Effect.gen(...)` wrap the function the name is given to.
  const isEffect = (callee: Node): boolean => (callee.type === "MemberExpression" && name(callee.object as Node) === "Effect")
    || (callee.type === "CallExpression" && isEffect(callee.callee as Node));
  let up = fn.parent;
  while (up?.type === "CallExpression" && isEffect(up.callee as Node)) up = up.parent;
  if (up?.type === "VariableDeclarator") return name(up.id as Node);
  if (up?.type === "Property") return name(up.key as Node);
  return null;
}

const calleeName = (call: Node) => {
  const callee = call.callee as Node;
  return callee.type === "MemberExpression" && !callee.computed ? name(callee.property as Node) : name(callee);
};

/** Every call of a function named `fn`, across src. */
const callsOf = (fn: string) => files.flatMap((file) => file.nodes.filter((node) => node.type === "CallExpression" && calleeName(node) === fn).map((call) => ({ file, call })));

/** The tables an expression stands for: a table by its name, or through a `const` or a parameter its callers fill. */
function tablesOf(node: Node | undefined, local: Map<string, string>, depth = 0): string[] {
  const named = name(node);
  if (!node || !named || depth > 3) return [];
  const table = local.get(named);
  if (table) return [table];
  const declared = declaration(node);
  if (declared === null) return [];
  if ("init" in declared) return declared.init ? tablesOf(declared.init, local, depth + 1) : [];
  return callsOf(declared.fn).flatMap(({ file, call }) => tablesOf((call.arguments as Node[])[declared.index], tableNames(file), depth + 1));
}

/** The tables a `.set`/`.values`/`.onConflictDoUpdate` call writes: the `update(t)` or `insert(t)` down its chain. */
function writtenTable(call: Node, local: Map<string, string>): string[] {
  for (let current: Node | undefined = call; current;) {
    if (current.type === "Identifier") {
      const declared = declaration(current);
      current = declared && "init" in declared ? declared.init ?? undefined : undefined;
      continue;
    }
    if (current.type !== "CallExpression") break;
    const method = calleeMethod(current);
    if (method === "update" || method === "insert") return tablesOf((current.arguments as Node[])[0], local);
    const callee = current.callee as Node;
    current = callee.type === "MemberExpression" ? (callee.object as Node) : undefined;
  }
  return [];
}

/** The write calls an object literal reaches: directly, through a `const` or a returning arrow, or by spread. */
function writesOf(object: Node, file: File, local: Map<string, string>, seen = new Set<Node>()): string[] {
  let target: Node = object;
  for (let up = object.parent; up; up = up.parent) {
    if (up.type === "CallExpression" && ["set", "values", "onConflictDoUpdate"].includes(calleeMethod(up) ?? "")) return writtenTable(up, local);
    if (up.type === "VariableDeclarator") {
      if (seen.has(up)) return [];
      seen.add(up);
      const named = name(up.id as Node);
      const uses = file.nodes.filter((node) => node.type === "Identifier" && node.name === named && node !== up.id
        && !(node.parent?.type === "MemberExpression" && node.parent.property === node && !node.parent.computed)
        && !(node.parent?.type === "Property" && node.parent.key === node && !node.parent.computed));
      return [...new Set(uses.flatMap((use) => writesOf(use, file, local, seen)))];
    }
    if (up.type === "ObjectExpression" && target.type !== "Property" && target.type !== "SpreadElement") break;
    if (up.type === "FunctionDeclaration" || up.type === "ExpressionStatement") break;
    target = up;
  }
  return [];
}

function addedLines(path: string): Set<number> | null {
  if (base === undefined) return null;
  const diff = execFileSync("git", ["diff", "-U0", base, "--", path], { cwd: root, encoding: "utf8" });
  const lines = new Set<number>();
  for (const match of diff.matchAll(/^@@ -\S+ \+(\d+)(?:,(\d+))? @@/gmu)) {
    const start = Number(match[1]);
    for (let i = 0; i < Number(match[2] ?? 1); i++) lines.add(start + i);
  }
  return lines;
}

// Moved columns: a `field: sql\`now()...\`` property in a write, optionally only on a line the diff added.
const moved = new Map<string, Column & { writes: string[] }>();
const unresolved: string[] = [];
for (const file of files) {
  const local = tableNames(file);
  const added = addedLines(file.path);
  for (const node of file.nodes) {
    if (node.type !== "Property" || !isSqlTemplate(node.value as Node)) continue;
    if (!postgresClock.test(sqlText((node.value as Node).quasi as Node))) continue;
    const line = file.lineOf(node.start);
    if (added && !added.has(line)) continue;
    const field = name(node.key as Node);
    if (!field) continue;
    const owners = writesOf(node.parent!, file, local);
    const projected = (() => { for (let up = node.parent; up; up = up.parent) if (up.type === "CallExpression" && ["select", "selectDistinct", "returning"].includes(calleeMethod(up) ?? "")) return true; return false; })();
    if (owners.length === 0 && !projected) unresolved.push(`${relative(root, file.path)}:${line} ${field}`);
    for (const owner of owners) {
      const snake = tables.get(owner)?.get(field);
      if (!snake) continue;
      const key = `${owner}.${field}`;
      const entry = moved.get(key) ?? { table: owner, field, snake, writes: [] };
      entry.writes.push(`${relative(root, file.path)}:${line}`);
      moved.set(key, entry);
    }
  }
}

const movedFields = new Set([...moved.values()].map((column) => column.field));
const movedSnakes = new Map([...moved.values()].map((column) => [column.snake, column]));
const fileOf = new Map<Node, File>();
for (const file of files) for (const node of file.nodes) fileOf.set(node, file);
const at = (node: Node) => {
  const file = fileOf.get(node)!;
  return `${relative(root, file.path)}:${file.lineOf(node.start)}`;
};

type Origin = "clock" | { fn: string; index: number } | null;

/** Where an expression's time comes from: the JS clock, a parameter its callers fill, or neither. */
function origin(node: Node, depth = 0): Origin {
  if (depth > 4) return null;
  switch (node.type) {
    case "CallExpression": {
      const callee = node.callee as Node;
      if (callee.type === "MemberExpression" && name(callee.object as Node) === "Date" && name(callee.property as Node) === "now") return "clock";
      if (callee.type === "MemberExpression" && ["getTime", "valueOf"].includes(name(callee.property as Node) ?? "")) return origin(callee.object as Node, depth + 1);
      return null;
    }
    case "NewExpression": {
      if (name(node.callee as Node) !== "Date") return null;
      const [first] = node.arguments as Node[];
      return first === undefined ? "clock" : origin(first, depth + 1);
    }
    case "MemberExpression":
      return name(node.object as Node) === "Clock" ? "clock" : null;
    case "BinaryExpression":
      return origin(node.left as Node, depth + 1) ?? origin(node.right as Node, depth + 1);
    case "TSAsExpression":
    case "TSNonNullExpression":
    case "ParenthesizedExpression":
      return origin(node.expression as Node, depth + 1);
    case "Identifier": {
      const declared = declaration(node);
      if (declared === null) return null;
      if ("init" in declared) return declared.init ? origin(declared.init, depth + 1) : null;
      return /^(now|nowMs|cutoff|since|deadline|at)$/iu.test(node.name as string) ? declared : null;
    }
    default:
      return null;
  }
}

/** Deciders behind an origin: here when it is the clock, else at each call that passes the clock for the parameter. */
function deciders(found: Origin, here: Node, why: string, depth = 0): Array<{ verdict: Verdict; at: string; note: string }> {
  if (found === null || depth > 3) return [];
  if (found === "clock") return [{ verdict: "js-clock", at: at(here), note: why }];
  return callsOf(found.fn).flatMap(({ call }) => {
    const argument = (call.arguments as Node[])[found.index];
    return argument ? deciders(origin(argument), call, `${why}, via ${found.fn}()`, depth + 1) : [];
  });
}

const comparison = new Set(["<", "<=", ">", ">=", "-", "===", "!==", "==", "!="]);
const drizzleCompare = new Set(["eq", "ne", "gt", "gte", "lt", "lte", "between", "notBetween"]);
const dateFnsCompare = /^(isAfter|isBefore|isPast|isFuture|differenceIn\w+|compareAsc|compareDesc)$/u;

/** Climb a read through `.getTime()`, `Number()`, `+x`, `?.`, `??`, `!` and parentheses to the expression that uses it. */
function usage(node: Node): Node {
  let current = node;
  for (;;) {
    const up = current.parent!;
    if (up.type === "MemberExpression" && up.object === current && ["getTime", "valueOf"].includes(name(up.property as Node) ?? "")
      && up.parent?.type === "CallExpression") { current = up.parent; continue; }
    if (up.type === "CallExpression" && name(up.callee as Node) === "Number") { current = up; continue; }
    if (up.type === "NewExpression" && name(up.callee as Node) === "Date" && (up.arguments as Node[]).length === 1) { current = up; continue; }
    if (up.type === "UnaryExpression" && up.operator === "+") { current = up; continue; }
    if (up.type === "LogicalExpression" && up.left === current) { current = up; continue; }
    if (["ChainExpression", "TSNonNullExpression", "ParenthesizedExpression", "TSAsExpression"].includes(up.type)) { current = up; continue; }
    return current;
  }
}

const touchesRow = (node: Node): boolean =>
  (node.type === "MemberExpression" && (movedFields.has(name(node.property as Node) ?? "") || name(node.property as Node) === "now"))
  || children(node).some(touchesRow);

type Judged = { verdict: Verdict; at: string; note: string };

/** How a JS read of a row time is used, following arithmetic, `const`s and returns to the comparison that decides. */
function judgeJs(read: Node, depth = 0): Judged[] {
  const used = usage(read);
  const up = used.parent!;
  const quiet = (note: string): Judged[] => [{ verdict: "display", at: at(read), note }];
  if (depth > 3) return quiet("followed too far");
  if (up.type === "BinaryExpression" && comparison.has(up.operator as string) || up.type === "BinaryExpression" && up.operator === "+") {
    const other = (up.left === used ? up.right : up.left) as Node;
    const found = deciders(origin(other), up, "compared with the JS clock");
    if (found.length > 0) return found;
    if (touchesRow(other)) return [{ verdict: "row-vs-row", at: at(up), note: "compared with another Postgres time" }];
    if (up.operator === "+" || up.operator === "-") return judgeJs(up, depth + 1);
    return quiet("compared with a non-clock value");
  }
  if (up.type === "CallExpression" && dateFnsCompare.test(calleeName(up) ?? "")) {
    const others = (up.arguments as Node[]).filter((argument) => argument !== used);
    if (others.length === 0) return [{ verdict: "js-clock", at: at(up), note: `${calleeName(up)}() reads the JS clock` }];
    const found = others.flatMap((other) => deciders(origin(other), up, `${calleeName(up)}() against the JS clock`));
    return found.length > 0 ? found : quiet(`${calleeName(up)}() against a non-clock value`);
  }
  if (depth > 0 && (up.type === "ReturnStatement" || up.type === "ArrowFunctionExpression")) {
    let fn: Node | null | undefined = up;
    while (fn && fn.type !== "ArrowFunctionExpression" && fn.type !== "FunctionExpression" && fn.type !== "FunctionDeclaration") fn = fn.parent;
    const named = fn ? functionName(fn) : null;
    if (!named) return quiet("returned from an unnamed function");
    const found = callsOf(named).flatMap(({ call }) => judgeJs(call, depth + 1)).filter((judged) => judged.verdict !== "display");
    return found.length > 0 ? found : quiet(`returned from ${named}()`);
  }
  if (depth > 0 && up.type === "VariableDeclarator" && up.init === used) {
    const named = name(up.id as Node);
    const scope = fileOf.get(up)!;
    const found = scope.nodes.filter((node) => node.type === "Identifier" && node.name === named && node !== up.id
      && up.parent!.parent && within(up.parent!.parent, node)).flatMap((use) => judgeJs(use, depth + 1)).filter((judged) => judged.verdict !== "display");
    return found.length > 0 ? found : quiet(`held in ${named}`);
  }
  return quiet(up.type);
}

const within = (outer: Node, inner: Node) => outer.start <= inner.start && inner.end <= outer.end;

/** A column in SQL: decided by Postgres when its comparison or template names now(), by JS when it takes a clock value. */
function judgeSql(node: Node): Judged[] {
  const quiet = (note: string): Judged[] => [{ verdict: "display", at: at(node), note }];
  for (let up = node.parent; up; up = up.parent) {
    if (up.type === "CallExpression" && drizzleCompare.has(calleeName(up) ?? "")) {
      const others = (up.arguments as Node[]).filter((argument) => !within(argument, node));
      if (others.some((other) => isSqlTemplate(other) && postgresClock.test(sqlText(other.quasi as Node)))) return [{ verdict: "sql-clock", at: at(node), note: calleeName(up)! }];
      const found = others.flatMap((other) => deciders(origin(other), up, `${calleeName(up)} against the JS clock`));
      return found.length > 0 ? found : quiet(`${calleeName(up)} against a non-clock value`);
    }
    if (isSqlTemplate(up) || up.type === "TemplateLiteral") {
      const template = up.type === "TemplateLiteral" ? up : (up.quasi as Node);
      if (postgresClock.test(sqlText(template))) return [{ verdict: "sql-clock", at: at(node), note: "template names now()" }];
      const holes = (template.expressions as Node[]).filter((hole) => !within(hole, node) && !touchesRow(hole));
      const found = holes.flatMap((hole) => deciders(origin(hole), template, "template takes the JS clock"));
      return found.length > 0 ? found : quiet("template without a clock");
    }
    if (up.type === "Property" || up.type === "ExpressionStatement" || up.type === "VariableDeclarator") return quiet("projection, order or write");
  }
  return quiet("outside a comparison");
}

const readers: Reader[] = [];
for (const file of files) {
  const local = tableNames(file);
  for (const node of file.nodes) {
    if (node.type === "MemberExpression" && !node.computed) {
      const field = name(node.property as Node);
      if (!field || !movedFields.has(field)) continue;
      const parent = node.parent!;
      if (parent.type === "AssignmentExpression" && parent.left === node) continue;
      const tablesHere = tablesOf(node.object as Node, local);
      const column = tablesHere.find((table) => moved.has(`${table}.${field}`));
      if (tablesHere.length > 0) {
        if (column === undefined) continue;
        for (const judged of judgeSql(node)) readers.push({ column: `${column}.${field}`, via: "drizzle", ...judged });
      } else {
        for (const judged of judgeJs(node)) readers.push({ column: `*.${field}`, via: "row", ...judged });
      }
    }
    if (node.type === "TemplateElement") {
      const raw = (node.value as { raw: string }).raw;
      for (const [snake, column] of movedSnakes) {
        if (!new RegExp(`\\b${snake}\\b`, "u").test(raw)) continue;
        for (const judged of judgeSql(node)) readers.push({ column: `*.${column.field} (${snake})`, via: "sql", ...judged });
      }
    }
    // A JS-clock write of a moved column's field: the column then holds times from two clocks.
    // Only a write whose table is a moved column's, or that can't be traced to a table (an object handed to a helper).
    const field = node.type === "Property" ? name(node.key as Node) : null;
    const owners = field && movedFields.has(field) && origin(node.value as Node) === "clock" ? writesOf(node.parent!, file, local) : null;
    if (owners && (owners.length === 0 || owners.some((table) => moved.has(`${table}.${field}`)))) {
      readers.push({ column: `*.${name(node.key as Node)}`, at: at(node), via: "write", verdict: "js-write", note: "written from the JS clock" });
    }
  }
}

/**
 * Deciders that stay on the JS clock, by file and field, each with why. Name the reason a reviewer would ask for; a new
 * decider that fits none of these is a bug to fix, not a line to add.
 */
const KEPT: ReadonlyArray<{ file: string; field: string; reason: string }> = [
  { file: "src/modules/agent/agent-runner.server.ts", field: "startedAt", reason: "an agent run record's start, epoch ms in jsonb and a bigint column, not a timestamp Postgres stamps" },
  { file: "src/modules/agent/remote-mcp.server.ts", field: "startedAt", reason: "an agent run record's start, epoch ms in jsonb and a bigint column, not a timestamp Postgres stamps" },
  { file: "src/modules/github/github-connect.server.ts", field: "expiresAt", reason: "a signed state token's expiry, issued and checked on the web clock, never stored" },
  { file: "src/modules/github/inngest-sync/sync.ts", field: "syncedAt", reason: "the GitHub repository cache's sync time, a column this change doesn't move and nothing ages" },
  { file: "src/modules/identity/auth.server.ts", field: "expiresAt", reason: "better-auth writes session expiry on the web clock; days long, so drift can't decide it" },
  { file: "src/modules/identity/organization-token.server.ts", field: "expiresAt", reason: "Organization token expiry, deferred: https://github.com/getployz/ployz/issues/4560" },
  { file: "src/modules/sandbox/anonymous-signup.server.ts", field: "expiresAt", reason: "Organization token expiry, deferred: https://github.com/getployz/ployz/issues/4560" },
  { file: "src/modules/sandbox/claim.server.ts", field: "expiresAt", reason: "Organization token expiry, deferred: https://github.com/getployz/ployz/issues/4560" },
  { file: "src/modules/ingress/cloudflare.setup.server.ts", field: "updatedAt", reason: "a fence, not a time: the put-back matches this exact value, which a microsecond now() would not survive" },
  { file: "useCanvasPositionMutation.ts", field: "updatedAt", reason: "the browser's optimistic copy; the server's write stamps now()" },
];

function excused(reader: Reader): string | null {
  const field = reader.column.split(".")[1]?.split(" ")[0];
  return KEPT.find((kept) => reader.at.split(":")[0]!.endsWith(kept.file) && kept.field === field)?.reason ?? null;
}

const unique = [...new Map(readers.map((reader) => [`${reader.column} ${reader.at} ${reader.verdict}`, reader])).values()];
console.log(`## Columns written with now()${base ? ` by the diff against ${base}` : ""}: ${moved.size}\n`);
console.log("| Column | SQL | Writes |\n|---|---|---|");
for (const column of [...moved.values()].sort((a, b) => `${a.table}.${a.field}`.localeCompare(`${b.table}.${b.field}`))) {
  console.log(`| ${column.table}.${column.field} | ${column.snake} | ${column.writes.join(", ")} |`);
}
if (unresolved.length > 0) console.log(`\nWrites whose table wasn't found: ${unresolved.join(", ")}`);

const counts = new Map<Verdict, number>();
for (const reader of unique) counts.set(reader.verdict, (counts.get(reader.verdict) ?? 0) + 1);
console.log(`\n## Readers and writers: ${unique.length}\n`);
console.log("| Verdict | Count |\n|---|---|");
for (const verdict of ["sql-clock", "row-vs-row", "display", "js-clock", "js-write"] as const) console.log(`| ${verdict} | ${counts.get(verdict) ?? 0} |`);

const decide = unique.filter((reader) => reader.verdict === "js-clock" || reader.verdict === "js-write");
const open = decide.filter((reader) => excused(reader) === null);
console.log(`\n## JS-clock deciders and writers: ${decide.length}, unexplained: ${open.length}\n`);
console.log("| Column | At | Via | Verdict | Note |\n|---|---|---|---|---|");
for (const reader of showAll ? unique : decide) {
  const reason = excused(reader);
  console.log(`| ${reader.column} | ${reader.at} | ${reader.via} | ${reader.verdict} | ${reason ? `kept: ${reason}` : reader.note} |`);
}
process.exitCode = open.length === 0 ? 0 : 1;

// now() is when the transaction began. One a function evaluates after it took a lock may predate the lock's wait.
const lockLike = (node: Node) => {
  if (node.type === "CallExpression" && calleeMethod(node) === "for") return true;
  if (node.type === "CallExpression" && /lock/iu.test(calleeName(node) ?? "")) return true;
  return isSqlTemplate(node) && /\bfor (update|share|no key update|key share)\b|pg_advisory/iu.test(sqlText(node.quasi as Node));
};
const afterLock: string[] = [];
const nowTemplate = (node: Node) => isSqlTemplate(node) && /\bnow\(\)/u.test(sqlText(node.quasi as Node));
for (const file of files) {
  const added = addedLines(file.path);
  // A module-level `const expired = sql\`... now()\`` is read where it is used: each use counts as its now().
  const nowConsts = new Set(file.nodes.filter((node) => node.type === "VariableDeclarator" && node.init && nowTemplate(node.init as Node)
    && node.parent?.parent?.type !== "BlockStatement").map((node) => name(node.id as Node)));
  for (const node of file.nodes) {
    const usesConst = node.type === "Identifier" && nowConsts.has(node.name as string) && node.parent?.type !== "VariableDeclarator"
      && !(node.parent?.type === "MemberExpression" && node.parent.property === node);
    if (!nowTemplate(node) && !usesConst) continue;
    if (added && !added.has(file.lineOf(node.start)) && !usesConst) continue;
    let top: Node = node;
    while (top.parent && top.parent.type !== "Program" && top.parent.type !== "ExportNamedDeclaration") top = top.parent;
    if (top.type === "VariableDeclaration" && nowTemplate(((top.declarations as Node[])[0]?.init as Node | undefined) ?? top)) continue;
    // A lock taken earlier in the declaration, or by the statement that reads now() itself (its projection is
    // evaluated before the row lock is granted).
    const lock = file.nodes.find((other) => within(top, other) && lockLike(other)
      && (other.end <= node.start || (calleeMethod(other) === "for" && within(other, node))));
    if (lock) afterLock.push(`| ${at(node)}${usesConst ? ` (${node.name})` : ""} | ${at(lock)}${within(lock, node) ? " (same statement)" : ""} |`);
  }
}
console.log(`\n## now() after a lock in the same declaration: ${new Set(afterLock).size}\n`);
console.log("| now() | Lock |\n|---|---|");
for (const line of new Set(afterLock)) console.log(line);
