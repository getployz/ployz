import { Schema } from "effect";

export const sessionAdditionalFields = {
  activeOrganizationSlug: { type: "string", required: false, input: false },
  sidebarOpen: { type: "boolean", required: true, defaultValue: true, validator: { input: Schema.toStandardSchemaV1(Schema.Boolean) } },
} as const;

/** Per-user preferences. `openStartedDeployments`: a manual Deploy opens its attempt on the canvas. */
export const userAdditionalFields = {
  openStartedDeployments: { type: "boolean", required: true, defaultValue: true, validator: { input: Schema.toStandardSchemaV1(Schema.Boolean) } },
} as const;
