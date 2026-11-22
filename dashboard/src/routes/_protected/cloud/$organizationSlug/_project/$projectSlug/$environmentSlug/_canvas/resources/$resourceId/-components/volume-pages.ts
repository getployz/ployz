import { Schema } from "effect";
import { tabSearchSchema } from "#/lib/tab-search";

export const VOLUME_PAGES = [
  { id: "copies", label: "Copies" },
  { id: "settings", label: "Settings" },
] as const;

export const volumePageSchema = Schema.Literals(VOLUME_PAGES.map((page) => page.id));

/** The open tab, which falls back to Copies. */
export const volumeSearchSchema = tabSearchSchema(VOLUME_PAGES, "copies");
