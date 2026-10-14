import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { user } from "#/modules/identity/tables";
import { queueMail } from "#/modules/mail/mail.inngest";
import type { MailMessage } from "#/modules/mail/mailer.server";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

export interface InviteMail {
  readonly to: string;
  readonly organization: string;
  readonly inviter: string;
  readonly role: "admin" | "member";
  readonly link: string;
  readonly expiresAt: Date;
}

const SANS = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif";

const escapeHtml = (value: string) =>
  value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;").replace(/'/g, "&#39;");

const article = (role: InviteMail["role"]) => (role === "admin" ? "an admin" : "a member");

const longDate = (date: Date) =>
  date.toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric", timeZone: "UTC" });

/** Names come from users and OAuth providers; a line break in one would forge a header or a body line. */
const oneLine = (name: string) => name.replace(/[\p{Cc}\p{Zl}\p{Zp}]+/gu, " ").trim();

export const renderInviteMail = (input: InviteMail): MailMessage => {
  const mail = { ...input, organization: oneLine(input.organization), inviter: oneLine(input.inviter) };
  const title = `Join ${mail.organization} on Ployz`;
  const body = `${mail.inviter} invited you to join ${mail.organization} as ${article(mail.role)}. The link works once and expires on ${longDate(mail.expiresAt)}.`;
  const action = `Join ${mail.organization}`;
  const reason = `${mail.inviter} sent this invite to ${mail.to}. If you weren't expecting it, ignore this email.`;
  const text = [title, "", body, "", `${action}: ${mail.link}`, "", "--", reason, ""].join("\n");
  const html = `<!doctype html>
<html><body style="margin:0;padding:0;background:#ffffff">
<div style="max-width:520px;margin:0 auto;padding:40px 32px;font-family:${SANS};color:#111">
<p style="margin:0 0 32px;font-size:15px;font-weight:600">Ployz</p>
<h1 style="margin:0 0 16px;font-size:22px;font-weight:600">${escapeHtml(title)}</h1>
<p style="margin:0 0 24px;font-size:15px;line-height:24px;color:#444">${escapeHtml(body)}</p>
<p style="margin:0 0 32px"><a href="${escapeHtml(mail.link)}" style="display:inline-block;background:#111;color:#ffffff;border-radius:8px;padding:11px 18px;font-size:15px;font-weight:600;text-decoration:none">${escapeHtml(action)}</a></p>
<hr style="border:none;border-top:1px solid #e6e6e6;margin:0 0 16px">
<p style="margin:0;font-size:12px;line-height:18px;color:#666">${escapeHtml(reason)}</p>
</div>
</body></html>`;
  return { to: mail.to, subject: `${mail.inviter} invited you to ${mail.organization} on Ployz`, text, html };
};

export const inviteMailKey = (invitationId: string) => `invitation-${invitationId}`;

export const emailInvite = Effect.fn("Invitation.email")(function* (
  invite: { readonly id: string; readonly email: string | null; readonly role: InviteMail["role"]; readonly expiresAt: Date },
  organizationId: string,
  inviterId: string,
  link: string,
) {
  const config = yield* AppConfig;
  if (config.mail === null || invite.email === null) return null;
  const { drizzle } = yield* Database;
  const [names] = yield* drizzle.select({ organization: organization.name, inviter: user.name })
    .from(organization).innerJoin(user, eq(user.id, inviterId))
    .where(eq(organization.id, organizationId)).limit(1);
  if (names === undefined) return null;
  const message = renderInviteMail({ to: invite.email, ...names, role: invite.role, link, expiresAt: invite.expiresAt });
  return yield* queueMail(message, inviteMailKey(invite.id)).pipe(
    Effect.as(invite.email),
    Effect.catchTag("InngestEventSendError", (error) =>
      Effect.logWarning("Invite mail was not queued; the dialog still shows the link.", error).pipe(Effect.as(null))),
  );
});
