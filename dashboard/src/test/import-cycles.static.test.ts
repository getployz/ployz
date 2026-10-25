import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");

// Cloud's modules load in import order, so a cycle through the Organization Runtime leaves a module half-initialized
// (a temporal dead zone on its Layer) for whichever side a test or route imports first. The runtime's frame recorder
// writes through the leaf `sealing-key-read.server`, never through `cluster-sealing.server`, which reads frames.
const KEPT_OUT_OF_CYCLES = [
  "modules/runtime/organization-runtime.server.ts",
  "modules/runtime/runtime-frame-record.server.ts",
  "modules/machines/sealing-ensure.server.ts",
  "modules/machines/sealing-key-read.server.ts",
];

const IMPORT = /^(?:import|export)\s+(type\s+)?(?:[^;'"]*?\sfrom\s+)?["']([^"']+)["']/gm;

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) return walk(path);
    return /\.tsx?$/.test(path) && !/\.(test|test-fixture|d)\.tsx?$/.test(path) ? [path] : [];
  });
}

function resolveImport(from: string, specifier: string): string | undefined {
  const base = specifier.startsWith("#/") ? join(SRC, specifier.slice(2))
    : specifier.startsWith(".") ? resolve(dirname(from), specifier)
    : undefined;
  if (base === undefined) return undefined;
  return [base, `${base}.ts`, `${base}.tsx`, join(base, "index.ts"), join(base, "index.tsx")]
    .find((candidate) => existsSync(candidate) && statSync(candidate).isFile());
}

/** Runtime imports only; `import type` is erased and cannot form a load-order cycle. */
function importGraph(): Map<string, Set<string>> {
  const graph = new Map<string, Set<string>>();
  for (const file of walk(SRC)) {
    const imports = new Set<string>();
    for (const [, typeOnly, specifier = ""] of readFileSync(file, "utf8").matchAll(IMPORT)) {
      if (typeOnly || specifier === "") continue;
      const target = resolveImport(file, specifier);
      if (target !== undefined && target !== file) imports.add(target);
    }
    graph.set(file, imports);
  }
  return graph;
}

/** Tarjan's strongly connected components; every component of two or more modules is an import cycle. */
function cycles(graph: Map<string, Set<string>>): string[][] {
  let next = 0;
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const stack: string[] = [];
  const found: string[][] = [];
  const visit = (node: string): number => {
    const own = next;
    next += 1;
    index.set(node, own);
    low.set(node, own);
    stack.push(node);
    for (const target of graph.get(node) ?? []) {
      if (!graph.has(target)) continue;
      const seen = index.get(target);
      const reach = seen === undefined ? visit(target) : stack.includes(target) ? seen : undefined;
      if (reach !== undefined) low.set(node, Math.min(low.get(node) ?? own, reach));
    }
    const lowest = low.get(node) ?? own;
    if (lowest !== own) return lowest;
    const component = stack.splice(stack.indexOf(node));
    if (component.length > 1) found.push(component.map((path) => relative(SRC, path)).sort());
    return lowest;
  };
  for (const node of graph.keys()) if (!index.has(node)) visit(node);
  return found;
}

describe("Dashboard import cycles", () => {
  const found = cycles(importGraph());

  it.each(KEPT_OUT_OF_CYCLES)("keep %s out of every import cycle", (module) => {
    expect(existsSync(join(SRC, module))).toBe(true);
    expect(found.filter((cycle) => cycle.includes(module))).toEqual([]);
  });
});
