import assert from "node:assert/strict";
import { access, readdir, readFile } from "node:fs/promises";
import { extname, join, relative } from "node:path";

const root = process.cwd();
const codeExtensions = new Set([".cjs", ".js", ".jsx", ".mjs", ".ts", ".tsx"]);
const ignoredDirectories = new Set([
  ".agents",
  ".claude",
  ".codex",
  ".continue",
  ".cursor",
  ".git",
  ".impeccable",
  ".junie",
  ".output",
  ".tanstack",
  ".windsurf",
  "dist",
  "node_modules",
  "public",
]);

function packageName(specifier) {
  if (
    specifier.startsWith(".") ||
    specifier.startsWith("#/") ||
    specifier.startsWith("@/") ||
    specifier.startsWith("node:")
  ) {
    return null;
  }
  const parts = specifier.split("/");
  return specifier.startsWith("@") ? parts.slice(0, 2).join("/") : parts[0];
}

function importsOf(source) {
  const specifiers = [];
  const patterns = [
    /\bfrom\s*["']([^"']+)["']/g,
    /\bimport\s*["']([^"']+)["']/g,
    /\b(?:import|require)\s*\(\s*["']([^"']+)["']\s*\)/g,
  ];
  for (const pattern of patterns) {
    for (const match of source.matchAll(pattern)) {
      if (match[1]) specifiers.push(match[1]);
    }
  }
  return specifiers;
}

async function walk(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) continue;
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await walk(path)));
    else if (codeExtensions.has(extname(entry.name))) files.push(path);
  }
  return files;
}

function createSignal(name) {
  return { name, files: new Set(), occurrences: 0 };
}

async function inventory() {
  const manifest = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  const directDependencies = new Set([
    ...Object.keys(manifest.dependencies ?? {}),
    ...Object.keys(manifest.devDependencies ?? {}),
  ]);
  const dependencyUsage = new Map(
    [...directDependencies].map((name) => [name, createSignal(name)]),
  );
  const signals = {
    effect: createSignal("effect"),
    zod: createSignal("zod"),
    betterResult: createSignal("better-result or #/lib/result"),
    inngest: createSignal("inngest or #/modules/inngest"),
    serverFunctions: createSignal("createServerFn"),
    mockTests: createSignal("Vitest mocks and spies"),
    sourceInspectionTests: createSignal("tests that inspect source files"),
    legacyArchitecture: createSignal("legacy architecture imports and globals"),
  };
  let sourceFiles = 0;
  let testFiles = 0;
  let sourceLines = 0;
  let testLines = 0;

  for (const path of await walk(root)) {
    const file = relative(root, path).replaceAll("\\", "/");
    const source = await readFile(path, "utf8");
    const lines = source.split("\n").length;
    const isSource = file.startsWith("src/") && /\.(?:ts|tsx)$/.test(file);
    const isTest = /\.(?:test|spec)\.(?:ts|tsx)$/.test(file);
    if (isSource) {
      sourceFiles += 1;
      sourceLines += lines;
    }
    if (isTest) {
      testFiles += 1;
      testLines += lines;
    }

    const imports = importsOf(source);
    for (const specifier of imports) {
      const dependency = packageName(specifier);
      const usage = dependency ? dependencyUsage.get(dependency) : undefined;
      if (usage) {
        usage.files.add(file);
        usage.occurrences += 1;
      }
      const matches = [
        [signals.effect, isSource && (dependency === "effect" || dependency?.startsWith("@effect/"))],
        [signals.zod, isSource && dependency === "zod"],
        [signals.betterResult, isSource && (dependency === "better-result" || specifier === "#/lib/result")],
        [signals.inngest, isSource && (dependency === "inngest" || specifier.startsWith("#/modules/inngest"))],
        [
          signals.legacyArchitecture,
          isSource &&
            (specifier.startsWith("#/models/") ||
              specifier.startsWith("#/inggest/")),
        ],
      ];
      for (const [signal, matched] of matches) {
        if (!matched) continue;
        signal.files.add(file);
        signal.occurrences += 1;
      }
    }

    const textSignals = [
      [signals.serverFunctions, isSource ? /\bcreateServerFn\b/g : /$a/g],
      [signals.mockTests, isTest ? /\bvi\.(?:doMock|fn|mock|spyOn)\b/g : /$a/g],
      [
        signals.sourceInspectionTests,
        isTest && /\b(?:readFile|readFileSync)\b/.test(source)
          ? /\b(?:readFile|readFileSync)\b/g
          : /$a/g,
      ],
    ];
    for (const [signal, pattern] of textSignals) {
      const count = [...source.matchAll(pattern)].length;
      if (count === 0) continue;
      signal.files.add(file);
      signal.occurrences += count;
    }
    if (
      isSource &&
      /\b(?:AppError|SerializedResult|serializeHandler|standardSchema|bindTestDatabase)\b/u.test(
        source,
      )
    ) {
      signals.legacyArchitecture.files.add(file);
      signals.legacyArchitecture.occurrences += 1;
    }
  }

  const summarize = ({ name, files, occurrences }) => ({
    name,
    files: files.size,
    occurrences,
  });

  return {
    source: { files: sourceFiles, lines: sourceLines },
    tests: { files: testFiles, lines: testLines },
    migrationSignals: Object.values(signals).map(summarize),
    directDependencyUsage: [...dependencyUsage.values()]
      .map(summarize)
      .sort((left, right) => left.name.localeCompare(right.name)),
  };
}

if (process.argv.includes("--self-check")) {
  assert.equal(packageName("@effect/vitest"), "@effect/vitest");
  assert.equal(packageName("effect/Schema"), "effect");
  assert.equal(packageName("#/lib/result"), null);
  assert.deepEqual(importsOf('import "zod"; import { Effect } from "effect";').sort(), [
    "effect",
    "zod",
  ]);
  const manifest = JSON.parse(await readFile(join(root, "package.json"), "utf8"));
  const directDependencies = {
    ...manifest.dependencies,
    ...manifest.devDependencies,
  };
  for (const dependency of [
    "better-result",
    "zod",
    "drizzle-zod",
    "@t3-oss/env-core",
    "dotenv",
  ]) {
    assert.equal(directDependencies[dependency], undefined, `${dependency} is still direct`);
  }
  for (const directory of ["src/models", "src/inggest"]) {
    await assert.rejects(access(join(root, directory)), `${directory} still exists`);
  }
  const report = await inventory();
  const signal = (name) =>
    report.migrationSignals.find((candidate) => candidate.name === name);
  assert.equal(signal("zod")?.occurrences, 0, "direct Zod imports remain");
  assert.equal(
    signal("better-result or #/lib/result")?.occurrences,
    0,
    "Better Result imports remain",
  );
  assert.equal(
    signal("legacy architecture imports and globals")?.occurrences,
    0,
    "legacy architecture references remain",
  );
  console.log("effect migration inventory self-check passed");
} else {
  console.log(JSON.stringify(await inventory(), null, 2));
}
