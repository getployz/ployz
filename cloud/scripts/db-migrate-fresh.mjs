import { pathToFileURL } from "node:url";
import { Polar } from "@polar-sh/sdk";
import { Client } from "pg";

export async function resetFreshDatabase({
  databaseUrl,
  polarAccessToken,
  polarServer = "production",
  PolarClient = Polar,
  PostgresClient = Client,
} = {}) {
  if (!databaseUrl) {
    throw new Error("DATABASE_URL is required");
  }

  if (polarAccessToken && polarServer !== "sandbox") {
    throw new Error(
      `db-migrate-fresh can only delete Polar customers in sandbox. Current POLAR_SERVER=${polarServer}`,
    );
  }

  const client = new PostgresClient({
    connectionString: databaseUrl,
  });

  try {
    if (polarAccessToken) {
      const polar = new PolarClient({
        accessToken: polarAccessToken,
        server: polarServer,
      });
      const pages = await polar.customers.list({
        limit: 100,
      });

      for await (const page of pages) {
        await Promise.all(
          page.result.items.map((customer) =>
            polar.customers.delete({
              id: customer.id,
            }),
          ),
        );
      }
    }

    await client.connect();
    await client.query("DROP SCHEMA IF EXISTS drizzle CASCADE");
    await client.query("DROP SCHEMA IF EXISTS public CASCADE");
    await client.query("CREATE SCHEMA public");
    await client.query("GRANT ALL ON SCHEMA public TO postgres");
    await client.query("GRANT ALL ON SCHEMA public TO public");
  } finally {
    await client.end();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  await resetFreshDatabase({
    databaseUrl: process.env.DATABASE_URL,
    polarAccessToken: process.env.POLAR_ACCESS_TOKEN,
    polarServer: process.env.POLAR_SERVER ?? "production",
  });
}
