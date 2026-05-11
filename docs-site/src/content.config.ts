import { defineCollection, z } from "astro:content";
import { docsLoader } from "@astrojs/starlight/loaders";
import { docsSchema } from "@astrojs/starlight/schema";

export const collections = {
  docs: defineCollection({
    loader: docsLoader(),
    schema: docsSchema({
      extend: z.object({
        llms: z
          .object({
            include: z.boolean().default(true),
            summary: z.string().optional(),
          })
          .default({ include: true }),
      }),
    }),
  }),
};
