// Renders the sample email to .output/mail-preview/sample.{html,txt}, and with MAIL_TO set also sends it through
// PostShiba using the POSTSHIBA_* and MAIL_FROM variables. Run from dashboard/: pnpm mail:preview
import { mkdir, writeFile } from "node:fs/promises";
import { Effect, Redacted } from "effect";
import { renderMail } from "#/modules/mail/layout";
import { makePostShibaMailer } from "#/modules/mail/mailer.server";
import { sampleMail } from "#/modules/mail/sample";

const required = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set.`);
  return value;
};

export async function run() {
  const origin = process.env["APP_URL"] ?? "http://localhost:3000";
  const mail = await renderMail(sampleMail(origin), origin);
  const dir = ".output/mail-preview";
  await mkdir(dir, { recursive: true });
  await writeFile(`${dir}/sample.html`, mail.html);
  await writeFile(`${dir}/sample.txt`, mail.text);
  console.log(`Wrote ${dir}/sample.html and ${dir}/sample.txt`);

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
