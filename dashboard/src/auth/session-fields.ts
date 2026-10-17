import { Schema } from "effect";

export const sessionAdditionalFields = {
  activeOrganizationSlug: { type: "string", required: false, input: false },
} as const;

/**
 * Per-user preferences. `openStartedDeployments`: a manual Deploy opens its attempt on the canvas. `isAnonymous`: an
 * agent's sandbox user, which sign-in treats as nobody.
 */
export const userAdditionalFields = {
  openStartedDeployments: { type: "boolean", required: true, defaultValue: true, validator: { input: Schema.toStandardSchemaV1(Schema.Boolean) } },
  isAnonymous: { type: "boolean", required: false, defaultValue: false, input: false },
} as const;
