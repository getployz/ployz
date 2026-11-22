/** A path segment that goes into the link as written, for values that are already URL-safe. */
class Unescaped {
  constructor(readonly value: string) {}
}

export const unescaped = (value: string) => new Unescaped(value);

type Segment = string | Unescaped;

/**
 * A link into the dashboard. Plain segments are percent-encoded; `unescaped` ones are not. `search` is the query string
 * without its `?`, already encoded.
 */
export function cloudLink(origin: string, segments: readonly Segment[], search?: string): string {
  const path = segments.map((segment) => (segment instanceof Unescaped ? segment.value : encodeURIComponent(segment)));
  return `${origin}/cloud/${path.join("/")}${search === undefined || search === "" ? "" : `?${search}`}`;
}

/** The Server list, or one Server's page. Slug and id go in as written. */
export const serverLink = (origin: string, organizationSlug: string, machineId?: string) =>
  cloudLink(origin, [
    unescaped(organizationSlug),
    unescaped("~"),
    unescaped("servers"),
    ...(machineId === undefined ? [] : [unescaped(machineId)]),
  ]);

export const UNSUBSCRIBE_PATH = "/api/mail/unsubscribe";

export const unsubscribeLink = (origin: string, token: string) => `${origin}${UNSUBSCRIBE_PATH}?token=${token}`;
