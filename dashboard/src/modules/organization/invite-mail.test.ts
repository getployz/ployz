import { expect, it } from "vitest";
import { type InviteMail, inviteMail } from "#/modules/organization/invite-mail";

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
  const mail = inviteMail(invite({}));
  expect(mail.subject).toContain("Ada");
  expect(mail.subject).toContain("Acme");
  expect(mail.paragraphs[0]).toContain("member");
  expect(mail.paragraphs[0]).toContain("2026");
  expect(mail.action.url).toBe("https://cloud.example.test/invite/t");
  expect(mail.reason).toContain("bob@example.test");
  expect(inviteMail(invite({ role: "admin" })).paragraphs[0]).toContain("admin");
});

it("keeps names on one line in the subject and the body", () => {
  const mail = inviteMail(invite({ organization: "Acme\r\nBcc: eve@example.test", inviter: "Ada\u0000 Lovelace\u0085" }));
  expect(mail.subject).toBe("Ada Lovelace invited you to Acme Bcc: eve@example.test on Ployz");
  expect(mail.paragraphs[0]).toContain("Ada Lovelace invited you to join Acme Bcc: eve@example.test as a member.");
});
