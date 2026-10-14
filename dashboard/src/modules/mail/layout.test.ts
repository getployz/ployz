import { describe, expect, it } from "vitest";
import { renderMail, type MailContent } from "#/modules/mail/layout";
import { sampleMail } from "#/modules/mail/sample";

const ORIGIN = "https://cloud.example.com";

describe("renderMail", () => {
  it("writes the plain text from the same fields as the HTML", async () => {
    const mail = await renderMail(sampleMail(ORIGIN), `${ORIGIN}/`);

    expect(mail.subject).toBe("Down: api in shop / production");
    expect(mail.text).toBe(
      [
        "api is down",
        "",
        "The health check on api fails since 14:02 AEDT. We'll email again when it recovers.",
        "",
        `View incident: ${ORIGIN}/cloud/acme/shop/production/uptime`,
        "",
        "Service: api",
        "Environment: shop / production",
        "Check: GET https://api.acme.com/health",
        "Failed with: 502 Bad Gateway",
        "",
        "--",
        "You're getting this because you're a member of Acme on Ployz.",
        `Unsubscribe from uptime emails: ${ORIGIN}/unsubscribe?token=sample`,
        `Notification settings: ${ORIGIN}/cloud/account`,
      ].join("\n"),
    );
  });

  it("links the action, the unsubscribe and the settings, and loads the logo from this Cloud", async () => {
    const { html } = await renderMail(sampleMail(ORIGIN), ORIGIN);

    expect(html).toContain(`href="${ORIGIN}/cloud/acme/shop/production/uptime"`);
    expect(html).toContain(`href="${ORIGIN}/unsubscribe?token=sample"`);
    expect(html).toContain(`href="${ORIGIN}/cloud/account"`);
    expect(html).toContain(`src="${ORIGIN}/assets/logo-mark.png"`);
    expect(html).toContain("The health check fails since 14:02 AEDT.");
  });

  it("leaves out the unsubscribe link and the facts when the email has none", async () => {
    const { unsubscribe: _, facts: __, ...rest } = sampleMail(ORIGIN);
    const mail = await renderMail(rest, ORIGIN);

    expect(mail.html).not.toContain("Unsubscribe");
    expect(mail.html).not.toContain("Failed with");
    expect(mail.text).not.toContain("Unsubscribe");
    expect(mail.text).not.toContain("Service: api");
  });

  it("escapes names that come from users", async () => {
    const content: MailContent = { ...sampleMail(ORIGIN), title: "<script>alert(1)</script> is down" };
    const { html } = await renderMail(content, ORIGIN);

    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
});
