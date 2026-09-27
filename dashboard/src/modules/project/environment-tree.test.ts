import { expect, it } from "vitest";
import { environmentTree } from "./environment-tree";

const environment = (id: string, createdAt: number) => ({ id, createdAt: new Date(createdAt) });
const branch = (environmentId: string, parentEnvironmentId: string) => ({ environmentId, parentEnvironmentId });

it("lists root Environments first, each Branch under its Parent, siblings oldest first", () => {
  const tree = environmentTree(
    [environment("fix-api", 4), environment("staging", 2), environment("production", 1), environment("fix-web", 3), environment("try-cache", 5)],
    [branch("fix-web", "production"), branch("fix-api", "production"), branch("try-cache", "fix-web")],
  );
  expect(tree.map((node) => [node.environment.id, node.depth, node.parent?.id ?? null])).toEqual([
    ["production", 0, null],
    ["fix-web", 1, "production"],
    ["try-cache", 2, "fix-web"],
    ["fix-api", 1, "production"],
    ["staging", 0, null],
  ]);
});

it("reads a Branch whose Parent isn't listed as a root", () => {
  const tree = environmentTree([environment("fix-web", 1)], [branch("fix-web", "elsewhere")]);
  expect(tree).toEqual([{ environment: environment("fix-web", 1), depth: 0, parent: null }]);
});
