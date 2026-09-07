import { Schema } from "effect";
import {
  decodeStrict,
  type StringSchema,
} from "#/modules/environment-design/schema";

export type EnvironmentNodeType =
  | "service"
  | "variable_group"
  | (string & {});

export type EnvironmentNodeNameIdentity = {
  type: EnvironmentNodeType;
  id: string;
  name: string;
};

export type EnvironmentNodeIdentityRef = Pick<
  EnvironmentNodeNameIdentity,
  "type" | "id"
>;

export function normalizeEnvironmentNodeName(name: string) {
  return name.trim().toLowerCase();
}

export function isEnvironmentNodeNameTaken(
  name: string,
  nodes: EnvironmentNodeNameIdentity[],
  excludeNode?: EnvironmentNodeIdentityRef,
) {
  const normalizedName = normalizeEnvironmentNodeName(name);

  return nodes.some((node) => {
    if (
      excludeNode &&
      node.type === excludeNode.type &&
      node.id === excludeNode.id
    ) {
      return false;
    }

    return normalizeEnvironmentNodeName(node.name) === normalizedName;
  });
}

export function getDuplicateEnvironmentNodeNameMessage(name: string) {
  return `A node named "${name}" already exists in this environment.`;
}

export function createEnvironmentNodeNameSchema(input: {
  schema: StringSchema;
  nodes: EnvironmentNodeNameIdentity[];
  excludeNode: EnvironmentNodeIdentityRef;
}) {
  return input.schema.check(
    Schema.makeFilter<string>((name) =>
      isEnvironmentNodeNameTaken(name, input.nodes, input.excludeNode)
        ? getDuplicateEnvironmentNodeNameMessage(name)
        : undefined,
    ),
  );
}

function getNameWithRandomSuffix(name: string, suffix: string, maxLength: number) {
  const safeSuffix = suffix.replace(/[^a-z0-9]/giu, "").toLowerCase().slice(0, 4);
  const resolvedSuffix = safeSuffix.padEnd(4, "0");
  const maxBaseLength = maxLength - resolvedSuffix.length - 1;
  const baseName = name.trim().slice(0, maxBaseLength).trim();

  return `${baseName}-${resolvedSuffix}`;
}

export function resolveUniqueEnvironmentNodeName(input: {
  name: string;
  nodes: EnvironmentNodeNameIdentity[];
  schema: StringSchema;
  maxLength?: number;
  randomSuffix?: () => string;
}) {
  const name = decodeStrict(input.schema, input.name);

  if (!isEnvironmentNodeNameTaken(name, input.nodes)) return name;

  const maxLength = input.maxLength ?? 64;
  const randomSuffix =
    input.randomSuffix ?? (() => crypto.randomUUID().replaceAll("-", ""));

  for (let attempt = 0; attempt < 20; attempt += 1) {
    const candidate = decodeStrict(
      input.schema,
      getNameWithRandomSuffix(name, randomSuffix(), maxLength),
    );

    if (!isEnvironmentNodeNameTaken(candidate, input.nodes)) return candidate;
  }

  throw new Error(`Failed to create a unique environment node name from ${name}`);
}
