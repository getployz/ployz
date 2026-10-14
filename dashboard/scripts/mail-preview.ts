// Renders a sample email to .output/mail-preview/<name>.{html,txt}, and with MAIL_TO set also sends it through
// PostShiba using the POSTSHIBA_* and MAIL_FROM variables. MAIL_TEMPLATE picks the email, "sample" by default.
// Run from dashboard/: pnpm mail:preview
import { mkdir, writeFile } from "node:fs/promises";
import type { DeploymentView, GitSource } from "@ployz/sdk";
import { Effect, Redacted } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { renderMail, type MailContent } from "#/modules/mail/layout";
import { makePostShibaMailer } from "#/modules/mail/mailer.server";
import { sampleMail } from "#/modules/mail/sample";
import { deployFailedMail } from "#/modules/mail/templates/deploys/deploy-failed";

const required = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set.`);
  return value;
};

const templates: Record<string, (origin: string) => MailContent> = {
  sample: sampleMail,
  "deploy-failed": (origin) => ({
    ...deployFailedMail({
      origin,
      organization: { name: "Acme", slug: "acme" },
      deployment: asTestDouble<DeploymentView>()({
        id: "0e5b2c1a-7f3d-4a8e-9b61-2d4c8f0a1e37",
        environment: { id: "env", project: "shop", name: "production" },
        status: "failed",
        outcome: null,
        nodes: [{ type: "service", id: "svc-api", name: "api" }],
        builds: [{ service: "api", commit: "8a7ed6e9f0c1", status: "failed", message: "Build failed: npm ci exited with code 1" }],
      }),
      sources: [asTestDouble<GitSource>()({ service: "api", repository: "acme/shop", branch: "main", commit: "8a7ed6e9f0c1d2b3a4f5" })],
    }),
    unsubscribe: { label: "Unsubscribe from failed deploy emails", url: `${origin}/api/mail/unsubscribe?token=sample` },
  }),
};

export async function run() {
  const origin = new URL(process.env["APP_URL"] ?? "http://localhost:3000").origin;
  const name = process.env["MAIL_TEMPLATE"] ?? "sample";
  const template = templates[name];
  if (!template) throw new Error(`MAIL_TEMPLATE must be one of ${Object.keys(templates).join(", ")}.`);
  const mail = await renderMail(template(origin), origin);
  const dir = ".output/mail-preview";
  await mkdir(dir, { recursive: true });
  await writeFile(`${dir}/${name}.html`, mail.html);
  await writeFile(`${dir}/${name}.txt`, mail.text);
  console.log(`Wrote ${dir}/${name}.html and ${dir}/${name}.txt`);

  const to = process.env["MAIL_TO"];
  if (!to) return;
  const mailer = makePostShibaMailer(
    {
      apiKey: Redacted.make(required("POSTSHIBA_API_KEY")),
      teamId: required("POSTSHIBA_TEAM_ID"),
      clusterId: required("POSTSHIBA_CLUSTER_ID"),
      from: required("MAIL_FROM"),
    },
    globalThis.fetch,
  );
  const result = await Effect.runPromise(mailer.send({ to, ...mail }, `mail-preview-${crypto.randomUUID()}`));
  console.log(`Sent to ${to}:`, result);
}
