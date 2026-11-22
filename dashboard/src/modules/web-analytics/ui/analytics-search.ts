import { Effect, Option, Schema } from "effect";
import {
  ANALYTICS_VIEWS,
  type AnalyticsViewKind,
  DEFAULT_RANGE,
  DEFAULT_WINDOW,
  type RangeSpec,
  REQUEST_WINDOWS,
  type RequestWindow,
} from "./analytics-model";

const dropInvalid = () => Effect.succeed(Option.none());

/** The URL's search. A view or window this page doesn't know is dropped, so it opens the default. */
export const analyticsSearchSchema = Schema.Struct({
  view: Schema.optional(Schema.Literals(ANALYTICS_VIEWS).pipe(Schema.catchDecoding(dropInvalid))),
  range: Schema.optional(Schema.String),
  page: Schema.optional(Schema.String),
  window: Schema.optional(Schema.Literals(REQUEST_WINDOWS.map((option) => option.spec)).pipe(Schema.catchDecoding(dropInvalid))),
  service: Schema.optional(Schema.String),
});

/** What the page shows: the URL's search with its defaults filled in. */
export type AnalyticsSearch = {
  readonly view: AnalyticsViewKind;
  readonly range: RangeSpec;
  readonly page?: string;
  readonly window: RequestWindow;
  readonly service?: string;
};

export function withDefaults(raw: typeof analyticsSearchSchema.Type): AnalyticsSearch {
  return {
    view: raw.view ?? "web",
    range: raw.range ?? DEFAULT_RANGE,
    window: raw.window ?? DEFAULT_WINDOW,
    ...(raw.page !== undefined && { page: raw.page }),
    ...(raw.service !== undefined && raw.service !== "" && { service: raw.service }),
  };
}

/** The URL's search for `search`: only what differs from the defaults. */
export function toUrlSearch(search: AnalyticsSearch): typeof analyticsSearchSchema.Type {
  return {
    ...(search.view !== "web" && { view: search.view }),
    ...(search.range !== DEFAULT_RANGE && { range: search.range }),
    ...(search.page !== undefined && { page: search.page }),
    ...(search.window !== DEFAULT_WINDOW && { window: search.window }),
    ...(search.service !== undefined && { service: search.service }),
  };
}
