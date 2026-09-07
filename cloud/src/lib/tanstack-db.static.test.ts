import { describe, expect, it } from "vitest";
import { API } from "typescript/unstable/sync";
import type { Expression, SourceFile } from "typescript/unstable/ast";
import {
  isCallExpression,
  isPropertyAccessExpression,
} from "typescript/unstable/ast/is";

const virtualPropertyNames = [
  "$synced",
  "$origin",
  "$key",
  "$collectionId",
] as const;

describe("live-row schema boundaries", () => {
  it("rejects direct parser calls with typed TanStack DB rows", () => {
    const root = process.cwd();
    const configPath = `${root}/tsconfig.json`;
    const api = new API({ cwd: root });

    try {
      const snapshot = api.updateSnapshot({ openProject: configPath });
      try {
        const project = snapshot.getProject(configPath);
        if (!project) throw new Error(`TypeScript project not found: ${configPath}`);

        const candidates: Array<{
          argument: Expression;
          source: SourceFile;
        }> = [];

        for (const file of project.rootFiles) {
          if (!file.startsWith(`${root}/src/`) || file.endsWith(".d.ts")) continue;
          const source = project.program.getSourceFile(file);
          if (!source) continue;

          source.forEachChild(function visit(node) {
            if (
              isCallExpression(node) &&
              isPropertyAccessExpression(node.expression) &&
              (node.expression.name.text === "parse" ||
                node.expression.name.text === "safeParse")
            ) {
              const argument = node.arguments[0];
              if (argument && !isPropertyAccessExpression(argument)) {
                candidates.push({ argument, source });
              }
            }
            node.forEachChild(visit);
          });
        }

        const argumentTypes = project.checker.getTypeAtLocation(
          candidates.map(({ argument }) => argument),
        );
        const violations = candidates.flatMap(({ argument, source }, index) => {
          const type = argumentTypes[index];
          if (!type) return [];
          const properties = new Set(
            project.checker.getPropertiesOfType(type).map(({ name }) => name),
          );
          if (!virtualPropertyNames.every((name) => properties.has(name))) return [];

          const before = source.text.slice(0, argument.pos).split("\n");
          const column = (before[before.length - 1]?.length ?? 0) + 1;
          return [`${source.fileName}:${before.length}:${column}`];
        });

        expect(violations, "Use parseLiveQueryRow for TanStack DB live rows").toEqual(
          [],
        );
      } finally {
        snapshot.dispose();
      }
    } finally {
      api.close();
    }
  });
});
