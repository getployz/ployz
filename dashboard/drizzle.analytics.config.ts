import { defineConfig } from 'drizzle-kit'

const url = process.env["DATABASE_URL"]

if (!url) {
  throw new Error('DATABASE_URL is required to run Drizzle Kit')
}

export default defineConfig({
  dialect: 'postgresql',
  schema: './src/modules/web-analytics/tables.ts',
  out: './drizzle/analytics',
  schemaFilter: ['analytics'],
  migrations: {
    table: '__drizzle_migrations_analytics',
  },
  dbCredentials: {
    url,
  },
  verbose: true,
  strict: true,
})
