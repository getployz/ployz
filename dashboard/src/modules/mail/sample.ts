import type { MailContent } from "#/modules/mail/layout";

/** A made-up uptime email that exercises every part of the layout, for previews and test sends. */
export const sampleMail = (origin: string): MailContent => ({
  subject: "api is down in shop / production",
  preview: "Health checks have failed since 14:02 UTC.",
  title: "api is down",
  paragraphs: ["Health checks on api have failed since 14:02 UTC. We'll email you when it's back up."],
  action: { label: "View incident", url: `${origin}/cloud/acme/shop/production/uptime` },
  facts: [
    { label: "Environment", value: "shop / production" },
    { label: "Check", value: "GET https://api.acme.com/health", mono: true },
    { label: "Error", value: "502 Bad Gateway" },
  ],
  reason: "You're getting this as a member of Acme on Ployz.",
  unsubscribe: { label: "Turn off uptime emails", url: `${origin}/api/unsubscribe?token=sample` },
});
