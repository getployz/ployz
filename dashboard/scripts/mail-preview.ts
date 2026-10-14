// Renders every sample email to .output/mail-preview/<name>.{html,txt}. With MAIL_TO set it also sends them through
// PostShiba using the POSTSHIBA_* and MAIL_FROM variables: the names given as arguments, else all of them. Sends go only
// to nick@ployz.dev. Run from dashboard/: MAIL_TO=nick@ployz.dev pnpm mail:preview uptime-down
import { mkdir, writeFile } from "node:fs/promises";
import { Effect, Redacted } from "effect";
import { type MailContent, renderMail } from "#/modules/mail/layout";
import { makePostShibaMailer } from "#/modules/mail/mailer.server";
import { sampleMail } from "#/modules/mail/sample";
import { NOTIFICATION_TYPES } from "#/modules/notifications/notification-types";
import { uptimeAlertSamples } from "#/modules/uptime/alert-mail";

const ALLOWED_TO = "nick@ployz.dev";

const required = (name: string) => {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is not set.`);
  return value;
};

export async function run() {
  const origin = new URL(process.env["APP_URL"] ?? "http://localhost:3000").origin;
  const unsubscribe = { label: NOTIFICATION_TYPES.uptime.unsubscribe, url: `${origin}/api/mail/unsubscribe?token=sample` };
  const samples: Record<string, MailContent> = {
    sample: sampleMail(origin),
    ...Object.fromEntries(Object.entries(uptimeAlertSamples(origin)).map(([name, content]) => [name, { ...content, unsubscribe }])),
  };
  const dir = ".output/mail-preview";
  await mkdir(dir, { recursive: true });
  const rendered = new Map<string, Awaited<ReturnType<typeof renderMail>>>();
  for (const [name, content] of Object.entries(samples)) {
    const mail = await renderMail(content, origin);
    rendered.set(name, mail);
    await writeFile(`${dir}/${name}.html`, mail.html);
    await writeFile(`${dir}/${name}.txt`, mail.text);
    console.log(`Wrote ${dir}/${name}.html and ${dir}/${name}.txt`);
  }

  const to = process.env["MAIL_TO"];
  if (!to) return;
  if (to !== ALLOWED_TO) throw new Error(`Test sends go only to ${ALLOWED_TO}, not ${to}.`);
  const names = process.argv.slice(3).filter((arg) => !arg.startsWith("-"));
  const unknown = names.filter((name) => !rendered.has(name));
  if (unknown.length > 0) throw new Error(`No sample named ${unknown.join(", ")}. Samples: ${[...rendered.keys()].join(", ")}.`);
  const mailer = makePostShibaMailer(
    {
      apiKey: Redacted.make(required("POSTSHIBA_API_KEY")),
      teamId: required("POSTSHIBA_TEAM_ID"),
      clusterId: required("POSTSHIBA_CLUSTER_ID"),
      from: required("MAIL_FROM"),
    },
    globalThis.fetch,
  );
  for (const name of names.length > 0 ? names : [...rendered.keys()]) {
    const mail = rendered.get(name);
    if (mail === undefined) continue;
    const result = await Effect.runPromise(mailer.send({ to, ...mail }, `mail-preview-${crypto.randomUUID()}`));
    console.log(`Sent ${name} to ${to}:`, result);
  }
}
