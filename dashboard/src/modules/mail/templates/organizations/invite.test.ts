import { expect, it } from "vitest";
import { type InviteMail, inviteMail } from "#/modules/mail/templates/organizations/invite";

const invite = (overrides: Partial<InviteMail>): InviteMail => ({
  to: "bob@example.test",
  organization: "Acme",
  inviter: "Ada",
  role: "member",
  link: "https://cloud.example.test/invite/t",
  expiresAt: new Date("2026-10-21T12:00:00Z"),
  ...overrides,
});

it("names the inviter, the role and the expiry, and links the invite", () => {
  expect(inviteMail(invite({}))).toEqual({
    subject: "Ada invited you to Acme on Ployz",
    preview: "The link works once and expires on October 21, 2026.",
    title: "Join Acme on Ployz",
    paragraphs: ["Ada invited you to join Acme as a member. The link works once and expires on October 21, 2026."],
    action: { label: "Join Acme", url: "https://cloud.example.test/invite/t" },
    reason: "Ada sent this invite to bob@example.test. If you weren't expecting it, ignore this email.",
  });
  expect(inviteMail(invite({ role: "admin" })).paragraphs[0]).toContain("as an admin.");
});

it("keeps names on one line in the subject and the body", () => {
  const mail = inviteMail(invite({ organization: "Acme\r\nBcc: eve@example.test", inviter: "Ada\u0000 Lovelace\u0085" }));
  expect(mail.subject).toBe("Ada Lovelace invited you to Acme Bcc: eve@example.test on Ployz");
  expect(mail.paragraphs[0]).toContain("Ada Lovelace invited you to join Acme Bcc: eve@example.test as a member.");
});
