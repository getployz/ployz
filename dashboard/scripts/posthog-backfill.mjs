// One-off: send the users and Organizations that predate PostHog, so they appear before their next event.
// Safe to rerun: identify and groupIdentify only set properties.
import { Client } from "pg";
import { PostHog } from "posthog-node";

// POSTHOG_HOST unset: posthog-node's own default, US Cloud.
const { DATABASE_URL, POSTHOG_KEY, POSTHOG_HOST } = process.env;

if (!DATABASE_URL || !POSTHOG_KEY) {
  throw new Error("DATABASE_URL and POSTHOG_KEY are required");
}

const db = new Client({ connectionString: DATABASE_URL });
const posthog = new PostHog(POSTHOG_KEY, { host: POSTHOG_HOST });

try {
  await db.connect();
  const users = await db.query(`SELECT id, email, name, created_at FROM "user"`);
  for (const user of users.rows) {
    posthog.identify({
      distinctId: user.id,
      properties: {
        $set: { email: user.email, name: user.name },
        $set_once: { created_at: user.created_at.toISOString() },
      },
    });
  }
  const organizations = await db.query(`
    SELECT o.id, o.name, o.slug, o.created_at, b.has_active_subscription, b.cancel_at_period_end
    FROM organization o LEFT JOIN organization_billing_state b ON b.organization_id = o.id`);
  for (const organization of organizations.rows) {
    posthog.groupIdentify({
      groupType: "organization",
      groupKey: organization.id,
      properties: {
        name: organization.name,
        slug: organization.slug,
        created_at: organization.created_at.toISOString(),
        pro: organization.has_active_subscription ?? false,
        cancel_at_period_end: organization.cancel_at_period_end ?? false,
      },
    });
  }
  console.log(`Sent ${users.rowCount} users and ${organizations.rowCount} Organizations.`);
} finally {
  await posthog.shutdown();
  await db.end();
}
