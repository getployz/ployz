import { Client } from "pg";

const databaseUrl = process.env.DATABASE_URL;

if (!databaseUrl) {
  throw new Error("DATABASE_URL is required");
}

const client = new Client({
  connectionString: databaseUrl,
});

try {
  await client.connect();
  console.log("Connected to database, dropping schemas...");
  await client.query("DROP SCHEMA IF EXISTS drizzle CASCADE");
  await client.query("DROP SCHEMA IF EXISTS public CASCADE");
  await client.query("CREATE SCHEMA public");
  await client.query("GRANT ALL ON SCHEMA public TO postgres");
  await client.query("GRANT ALL ON SCHEMA public TO public");
  console.log("Schemas reset successfully");
} finally {
  await client.end();
}
