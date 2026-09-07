import { asString } from "#/lib/json";

export const ENVIRONMENT_RESOURCE_TYPES = [
  "variable_group",
  "volume",
] as const;

export type EnvironmentResourceType =
  (typeof ENVIRONMENT_RESOURCE_TYPES)[number];

const environmentResourceTypes = new Set<string>(ENVIRONMENT_RESOURCE_TYPES);

export function isEnvironmentResourceType<T>(
  value: T,
): value is T & EnvironmentResourceType {
  const text = asString(value);
  return text !== null && environmentResourceTypes.has(text);
}
