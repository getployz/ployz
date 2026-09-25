import { Effect, Option, Schema } from "effect";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "settings", label: "Settings" },
] as const;

/** The service panel's tabs in Deployment Mode: read-only, as the attempt deployed the service. */
export const DEPLOYMENT_SERVICE_PAGES = [
  { id: "details", label: "Details" },
  { id: "build-logs", label: "Build logs" },
  { id: "deploy-logs", label: "Deploy logs" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];
export type DeploymentServicePage = (typeof DEPLOYMENT_SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));
export const deploymentServicePageSchema = Schema.Literals(DEPLOYMENT_SERVICE_PAGES.map((page) => page.id));

/** The panel's tabs for the mode in the URL: Deployment Mode when `deployment` is set, else Editor Mode. */
export const servicePagesFor = (deployment: string | undefined) => deployment ? DEPLOYMENT_SERVICE_PAGES : SERVICE_PAGES;

export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(Schema.Literals([...SERVICE_PAGES, ...DEPLOYMENT_SERVICE_PAGES].map((page) => page.id)).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("settings" as const))),
  )),
});
