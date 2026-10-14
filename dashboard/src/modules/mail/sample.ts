import type { MailContent } from "#/modules/mail/layout";

/** A made-up uptime email that exercises every part of the layout, for previews and test sends. */
export const sampleMail = (origin: string): MailContent => ({
  subject: "Down: api in shop / production",
  preview: "The health check fails since 14:02 AEDT.",
  title: "api is down",
  paragraphs: ["The health check on api fails since 14:02 AEDT. We'll email again when it recovers."],
  action: { label: "View incident", url: `${origin}/cloud/acme/shop/production/uptime` },
  facts: [
    { label: "Service", value: "api" },
    { label: "Environment", value: "shop / production" },
    { label: "Check", value: "GET https://api.acme.com/health", mono: true },
    { label: "Failed with", value: "502 Bad Gateway" },
  ],
  reason: "You're getting this because you're a member of Acme on Ployz.",
  unsubscribe: { label: "Unsubscribe from uptime emails", url: `${origin}/api/unsubscribe?token=sample` },
});
