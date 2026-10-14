import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { deployFailedMail } from "#/modules/mail/templates/deploys/deploy-failed";
import { queueOrganizationMail } from "#/modules/notifications/recipients.server";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

export const mailFailedGithubDeploy = Effect.fn("StoreDeployment.mailFailed")(function* (data: ConfigDeploymentAdmittedEventData) {
  if (data.trigger !== "github") return { skipped: "not started by GitHub" };
  const store = yield* cloudStore;
  const deployment = yield* storeTry(() => store.read(data.organizationId, { query: "deployment", id: data.deploymentId }));
  if (deployment.status !== "failed" && deployment.status !== "unknown") return { skipped: deployment.status };
  const sources = yield* storeTry(() => store.deploymentSources(data.deploymentId));
  const { drizzle } = yield* Database;
  const [org] = yield* drizzle.select({ name: organization.name, slug: organization.slug }).from(organization)
    .where(eq(organization.id, data.organizationId)).limit(1);
  if (org === undefined) return { skipped: "organization gone" };
  const { app } = yield* AppConfig;
  const content = deployFailedMail({ origin: app.url.origin, organization: org, deployment, sources });
  const mailed = yield* queueOrganizationMail({
    organizationId: data.organizationId, type: "deploy-failed", key: `deployment:${data.deploymentId}:failed`, content,
  });
  return { mailed };
});
