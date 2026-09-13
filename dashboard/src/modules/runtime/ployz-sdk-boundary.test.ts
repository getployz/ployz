import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(process.cwd(), "src");
const VALUE_IMPORT_ALLOWLIST = new Set([
  "modules/runtime/ployz.server.ts",
  "models/runtime/ployz-sdk.server.test.ts",
]);
const BRANDED_CONSTRUCTORS = [
  "operationId",
  "machineId",
  "namespaceId",
  "serviceId",
  "volumeName",
  "imageReference",
  "replicaCount",
  "routeHostname",
  "routePort",
  "containerMountPath",
  "eventSequence",
  "operationEventReplayLimit",
  "operationIdempotencyKey",
  "cancellationReason",
  "imageDefaultRuntime",
  "connectPloyzNatsClient",
  "OPERATION_API_CONTRACTS",
];

function walk(dir: string): string[] {
  const entries = readdirSync(dir);
  const files: string[] = [];
  for (const entry of entries) {
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      files.push(...walk(path));
      continue;
    }
    if (path.endsWith(".ts") || path.endsWith(".tsx")) files.push(path);
  }
  return files;
}

type SdkImport = {
  line: number;
  statement: string;
  typeOnly: boolean;
  generated: boolean;
};

function sdkImports(source: string): SdkImport[] {
  const lines = source.split("\n");
  const matches: SdkImport[] = [];
  for (let i = 0; i < lines.length; i += 1) {
    const start = lines[i] ?? "";
    if (!/^\s*import\b/.test(start)) continue;
    let statement = start;
    let end = i;
    while (
      !/from\s+["'][^"']+["']/.test(statement) &&
      !/;\s*$/.test(statement) &&
      end + 1 < lines.length
    ) {
      end += 1;
      statement += `\n${lines[end] ?? ""}`;
    }
    if (!/from\s+["']@ployz\/sdk(?:\/generated)?["']/.test(statement)) {
      i = end;
      continue;
    }
    matches.push({
      line: i + 1,
      statement,
      typeOnly: /^\s*import\s+type\s/.test(statement),
      generated: statement.includes("@ployz/sdk/generated"),
    });
    i = end;
  }
  return matches;
}

describe("Ployz SDK call-site boundary", () => {
  const files = walk(SRC)
    .map((path) => ({
      path,
      rel: relative(SRC, path).replaceAll("\\", "/"),
      source: readFileSync(path, "utf8"),
    }))
    .filter((file) => file.rel !== "models/runtime/ployz-sdk-boundary.test.ts");

  it("does not import @ployz/sdk/generated", () => {
    const hits = files.flatMap((file) =>
      sdkImports(file.source)
        .filter((entry) => entry.generated)
        .map(
          (entry) =>
            `${file.rel}:${entry.line}: ${entry.statement.replaceAll("\n", " ")}`,
        ),
    );
    expect(hits).toEqual([]);
  });

  it("does not keep NATS client, branded constructors, or OPERATION_API_CONTRACTS", () => {
    const hits = files.flatMap((file) =>
      sdkImports(file.source)
        .filter((entry) => !entry.typeOnly)
        .flatMap((entry) =>
          BRANDED_CONSTRUCTORS.filter((name) =>
            new RegExp(`\\b${name}\\b`).test(entry.statement),
          ).map(
            (name) =>
              `${file.rel}:${entry.line}: ${name} via ${entry.statement.replaceAll("\n", " ")}`,
          ),
        ),
    );
    expect(hits).toEqual([]);
  });

  it("value-imports @ployz/sdk only from the adapter", () => {
    const hits = files.flatMap((file) => {
      if (VALUE_IMPORT_ALLOWLIST.has(file.rel)) return [];
      return sdkImports(file.source)
        .filter((entry) => !entry.typeOnly && !entry.generated)
        .map(
          (entry) =>
            `${file.rel}:${entry.line}: ${entry.statement.replaceAll("\n", " ")}`,
        );
    });
    expect(hits).toEqual([]);
  });
});
