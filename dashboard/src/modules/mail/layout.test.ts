import { describe, expect, it } from "vitest";
import { renderMail, type MailContent } from "#/modules/mail/layout";
import { sampleMail } from "#/modules/mail/sample";

const ORIGIN = "https://cloud.example.com";

describe("renderMail", () => {
  it("writes the plain text from the same fields as the HTML", async () => {
    const mail = await renderMail(sampleMail(ORIGIN), `${ORIGIN}/`);

    expect(mail.subject).toBe("api is down in shop / production");
    expect(mail.text).toBe(
      [
        "api is down",
        "",
        "Health checks on api have failed since 14:02 UTC. We'll email you when it's back up.",
        "",
        `View incident: ${ORIGIN}/cloud/acme/shop/production/uptime`,
        "",
        "Environment: shop / production",
        "Check: GET https://api.acme.com/health",
        "Error: 502 Bad Gateway",
        "",
        "--",
        "You're getting this as a member of Acme on Ployz.",
        `Turn off uptime emails: ${ORIGIN}/api/unsubscribe?token=sample`,
        `Manage emails: ${ORIGIN}/cloud/account`,
      ].join("\n"),
    );
  });

  it("links the action, the unsubscribe and the settings, and loads the logo from this Cloud", async () => {
    const { html } = await renderMail(sampleMail(ORIGIN), ORIGIN);

    expect(html).toContain(`href="${ORIGIN}/cloud/acme/shop/production/uptime"`);
    expect(html).toContain(`href="${ORIGIN}/api/unsubscribe?token=sample"`);
    expect(html).toContain(`href="${ORIGIN}/cloud/account"`);
    expect(html).toContain(`src="${ORIGIN}/assets/logo-mark.png"`);
    expect(html).toContain("Health checks have failed since 14:02 UTC.");
  });

  it("leaves out the unsubscribe link and the facts when the email has none", async () => {
    const { unsubscribe: _, facts: __, ...rest } = sampleMail(ORIGIN);
    const mail = await renderMail(rest, ORIGIN);

    expect(mail.html).not.toContain("Turn off");
    expect(mail.html).not.toContain("502 Bad Gateway");
    expect(mail.text).not.toContain("Turn off");
    expect(mail.text).not.toContain("Environment: shop / production");
  });

  it("escapes names that come from users", async () => {
    const content: MailContent = { ...sampleMail(ORIGIN), title: "<script>alert(1)</script> is down" };
    const { html } = await renderMail(content, ORIGIN);

    expect(html).not.toContain("<script>");
    expect(html).toContain("&lt;script&gt;");
  });
});
