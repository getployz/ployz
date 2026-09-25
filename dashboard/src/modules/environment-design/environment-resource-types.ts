export const ENVIRONMENT_RESOURCE_TYPES = [
  "volume",
] as const;

export type EnvironmentResourceType =
  (typeof ENVIRONMENT_RESOURCE_TYPES)[number];
