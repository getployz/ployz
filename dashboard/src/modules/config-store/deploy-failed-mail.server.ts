import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { ConfigDeploymentAdmittedEventData } from "#/modules/inngest/events";
import { deploymentTrouble } from "#/modules/config-store/store-deployments";
import { deployFailedMail, deployWaitingMail } from "#/modules/mail/templates/deploys/deploy-failed";
import { queueOrganizationMail } from "#/modules/notifications/recipients.server";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/**
 * Mails a Deploy GitHub started to the members who want it when it failed, or when it applied and left Services
 * waiting: nobody watched either end.
 */
export const mailGithubDeployTrouble = Effect.fn("StoreDeployment.mailTrouble")(function* (data: ConfigDeploymentAdmittedEventData) {
  if (data.trigger !== "github") return { skipped: "not started by GitHub" };
  const store = yield* cloudStore;
  const deployment = yield* storeTry(() => store.read(data.organizationId, { query: "deployment", id: data.deploymentId }));
  const trouble = deploymentTrouble(deployment);
  if (trouble === null) return { skipped: deployment.status };
  const sources = yield* storeTry(() => store.deploymentSources(data.deploymentId));
  const { drizzle } = yield* Database;
  const [org] = yield* drizzle.select({ name: organization.name, slug: organization.slug }).from(organization)
    .where(eq(organization.id, data.organizationId)).limit(1);
  if (org === undefined) return { skipped: "organization gone" };
  const { app } = yield* AppConfig;
  const mail = { origin: app.url.origin, organization: org, deployment, sources };
  const mailed = yield* queueOrganizationMail({
    organizationId: data.organizationId, type: "deploy-failed",
    key: `deployment:${data.deploymentId}:${trouble}`,
    content: trouble === "failed" ? deployFailedMail(mail) : deployWaitingMail(mail),
  });
  return { mailed };
});
