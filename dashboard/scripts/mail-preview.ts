// Renders every preview email to .output/mail-preview/<name>.{html,txt}, and with MAIL_TO set also sends the one
// MAIL_PREVIEW names (default "sample") through PostShiba using the POSTSHIBA_* and MAIL_FROM variables. Test sends
// only go to nick@ployz.dev. Run from dashboard/: pnpm mail:preview
import { mkdir, writeFile } from "node:fs/promises";
import { Effect, Redacted } from "effect";
import { renderMail, type MailContent, type RenderedMail } from "#/modules/mail/layout";
import { makePostShibaMailer } from "#/modules/mail/mailer.server";
import { sampleMail } from "#/modules/mail/sample";
import { upgradeFailedSamples } from "#/modules/mail/templates/servers/upgrade-failed";
import { NOTIFICATION_TYPES } from "#/modules/notifications/notification-types";

const TEST_RECIPIENT = "nick@ployz.dev";

const required = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set.`);
  return value;
};

const previews = (origin: string): Record<string, MailContent> => {
  const unsubscribe = (label: string) => ({ label, url: `${origin}/api/mail/unsubscribe?token=sample` });
  return {
    sample: sampleMail(origin),
    ...Object.fromEntries(
      Object.entries(upgradeFailedSamples(origin)).map(([name, content]) => [
        name,
        { ...content, unsubscribe: unsubscribe(NOTIFICATION_TYPES["upgrade-failed"].unsubscribe) },
      ]),
    ),
  };
};

export async function run() {
  const origin = new URL(process.env["APP_URL"] ?? "http://localhost:3000").origin;
  const dir = ".output/mail-preview";
  await mkdir(dir, { recursive: true });
  const rendered = new Map<string, RenderedMail>();
  for (const [name, content] of Object.entries(previews(origin))) {
    const mail = await renderMail(content, origin);
    rendered.set(name, mail);
    await writeFile(`${dir}/${name}.html`, mail.html);
    await writeFile(`${dir}/${name}.txt`, mail.text);
    console.log(`Wrote ${dir}/${name}.html and ${dir}/${name}.txt`);
  }

  const to = process.env["MAIL_TO"];
  if (!to) return;
  if (to !== TEST_RECIPIENT) throw new Error(`Test sends only go to ${TEST_RECIPIENT}.`);
  const name = process.env["MAIL_PREVIEW"] ?? "sample";
  const mail = rendered.get(name);
  if (!mail) throw new Error(`No preview named ${name}. Pick one of ${[...rendered.keys()].join(", ")}.`);
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
  console.log(`Sent ${name} to ${to}:`, result);
}
