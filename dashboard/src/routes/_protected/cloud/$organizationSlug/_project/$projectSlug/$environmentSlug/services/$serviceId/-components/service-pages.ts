import { Effect, Option, Schema } from "effect";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "settings", label: "Settings" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

/**
 * Old Deployment Mode links name these tabs; they must survive search validation so the canvas can send the link to its
 * Deployment Page (`legacyDeploymentLink`). No service tab uses them.
 */
const legacyDeploymentTabs = ["build-logs", "deploy-logs"] as const;

export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(Schema.Literals([...SERVICE_PAGES.map((page) => page.id), ...legacyDeploymentTabs]).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("settings" as const))),
  )),
});
