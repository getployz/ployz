import { Schema } from "effect";

export const sessionAdditionalFields = {
  activeOrganizationSlug: { type: "string", required: false, input: false },
  sidebarOpen: { type: "boolean", required: true, defaultValue: true, validator: { input: Schema.toStandardSchemaV1(Schema.Boolean) } },
} as const;
