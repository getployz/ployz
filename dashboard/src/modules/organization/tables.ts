import { createdAt } from "#/db/tables";

import { createSelectSchema } from "drizzle-orm/effect-schema";

import { pgTable, text, unique, uuid } from "drizzle-orm/pg-core";

import { Schema } from "effect";



const requiredTrimmedString = Schema.Trim.check(Schema.isNonEmpty());

export const organization = pgTable(
  "organization",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    name: text("name").notNull(),
    slug: text("slug").notNull(),
    logo: text("logo"),
    metadata: text("metadata"),
    createdAt,
  },
  (table) => [unique().on(table.slug)],
);

export const organizationSelectSchema = createSelectSchema(organization, {
  name: requiredTrimmedString,
  slug: requiredTrimmedString,
});

export const organizationSlugSchema = organizationSelectSchema.fields.slug;
