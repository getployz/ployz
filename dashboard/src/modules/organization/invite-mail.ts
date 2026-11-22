import type { MailContent } from "#/modules/mail/layout";

export type InviteMail = {
  readonly to: string;
  readonly organization: string;
  readonly inviter: string;
  readonly role: "admin" | "member";
  readonly link: string;
  readonly expiresAt: Date;
};

const article = (role: InviteMail["role"]) => (role === "admin" ? "an admin" : "a member");

const longDate = (date: Date) =>
  date.toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric", timeZone: "UTC" });

/** Names come from users and OAuth providers; a line break in one would forge a header or a body line. */
const oneLine = (name: string) => name.replace(/[\p{Cc}\p{Zl}\p{Zp}]+/gu, " ").trim();

/** An invite is transactional: no unsubscribe, since the person may not have an account yet. */
export function inviteMail(input: InviteMail): MailContent {
  const organization = oneLine(input.organization);
  const inviter = oneLine(input.inviter);
  const expires = `The link works once and expires on ${longDate(input.expiresAt)}.`;
  return {
    subject: `${inviter} invited you to ${organization} on Ployz`,
    preview: expires,
    title: `Join ${organization} on Ployz`,
    paragraphs: [`${inviter} invited you to join ${organization} as ${article(input.role)}. ${expires}`],
    action: { label: `Join ${organization}`, url: input.link },
    reason: `${inviter} sent this invite to ${input.to}. If you weren't expecting it, ignore this email.`,
  };
}

export const inviteSamples = (origin: string) => ({
  invite: inviteMail({
    to: "bob@acme.com",
    organization: "Acme",
    inviter: "Ada Lovelace",
    role: "member",
    link: `${origin}/invite/sample`,
    expiresAt: new Date("2026-10-21T12:00:00Z"),
  }),
});
