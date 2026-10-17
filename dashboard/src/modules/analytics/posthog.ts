import { type AnyRouter, createMemoryHistory } from "@tanstack/react-router";
import { createIsomorphicFn } from "@tanstack/react-start";
import posthog, { type CaptureResult, type Properties } from "posthog-js";
import type { AuthSession } from "#/auth/auth";

/** The public project key and where to send events; null when Cloud runs without PostHog. */
export type PostHogBrowserConfig = { readonly key: string; readonly host: string } | null;

/** Read during SSR and handed to the browser with the router's dehydrated state. */
export const getPostHogBrowserConfig = createIsomorphicFn()
  .client(async (): Promise<PostHogBrowserConfig> => null)
  .server(async (): Promise<PostHogBrowserConfig> => {
    const [{ postHogBrowserConfig }, { runAppEffect }] = await Promise.all([
      import("#/modules/analytics/posthog.server"),
      import("#/server/run.server"),
    ]);
    return runAppEffect(postHogBrowserConfig);
  });

/**
 * Pages whose URL is itself a secret, an invite, a claim link or a device code, mark their route `staticData.secret`.
 * PostHog records every URL, so nothing on them reaches it, and no later event carries their URL anywhere.
 * A path counts as secret exactly when the router would render one of those routes for it, encodings and all.
 */
let opensSecretPage = (_path: string) => true;

const secretPagesOf = (router: AnyRouter) => (path: string) => {
  const { pathname } = router.parseLocation(createMemoryHistory({ initialEntries: [path] }).location);
  return router.getMatchedRoutes(pathname)[0].some((route) => route.options.staticData?.secret === true);
};

/** An absolute URL or a rooted path; any other text, like a button's "Invite", is not a URL. */
const isSecretUrl = (value: string) =>
  value.startsWith("/") ? opensSecretPage(value) : URL.canParse(value) && opensSecretPage(new URL(value).pathname);

const JSON_STRING = /"(?:[^"\\]|\\.)*"/g;

/** Checks every string, key or value, however deep: rrweb nests URLs in DOM snapshots, heatmaps key by URL. */
const carriesSecretUrl = (value: readonly (Properties | undefined)[]) =>
  JSON.stringify(value).match(JSON_STRING)?.some((literal: string) => isSecretUrl(String(JSON.parse(literal)))) ?? false;

const withoutSecretUrls = (properties: Properties | undefined) =>
  properties === undefined ? undefined : Object.fromEntries(Object.entries(properties).filter(([, value]) => !isSecretUrl(String(value))));

/**
 * A referrer or previous page that was secret is stripped; a secret anywhere else, like a replay batch holding that
 * page's DOM, drops the whole event.
 */
function dropSecretUrls(event: CaptureResult | null): CaptureResult | null {
  if (event === null || opensSecretPage(window.location.pathname) || isSecretUrl(String(event.properties["$current_url"]))) return null;
  const stripped = {
    ...event,
    properties: withoutSecretUrls(event.properties) ?? {},
    $set: withoutSecretUrls(event.$set),
    $set_once: withoutSecretUrls(event.$set_once),
  };
  return carriesSecretUrl([stripped.properties, stripped.$set, stripped.$set_once]) ? null : stripped;
}

/**
 * Starts PostHog once per page load. Sign-in arrives through a full page load, so identifying here covers it.
 * Replays show inputs: anything secret carries `ph-no-capture` (see CODING_STANDARDS.md).
 */
export function startPostHog(config: PostHogBrowserConfig, session: AuthSession | null, router: AnyRouter) {
  opensSecretPage = secretPagesOf(router);
  if (config === null || opensSecretPage(window.location.pathname)) return;
  posthog.init(config.key, {
    api_host: config.host,
    // ponytail: the PostHog app's own origin, for links and the toolbar while api_host is the managed proxy.
    ui_host: "https://us.posthog.com",
    defaults: "2026-08-30",
    capture_exceptions: true,
    session_recording: { maskAllInputs: false, maskInputOptions: { password: true } },
    before_send: dropSecretUrls,
  });
  if (session !== null) {
    posthog.identify(session.user.id, { email: session.user.email, name: session.user.name });
  }
}

/**
 * Called before each client navigation: the replay stops for a secret page and stays off until the next page load.
 * Restarting on the way out would snapshot the secret page's DOM, still mounted when Back leaves it.
 * It stops even when not yet recording, since a replay waiting on remote config would otherwise start on the secret page.
 */
export function followPostHogNavigation(pathname: string) {
  if (posthog.__loaded && opensSecretPage(pathname)) posthog.stopSessionRecording();
}

export function setPostHogOrganization(organizationId: string, properties: { readonly name: string; readonly slug: string }) {
  if (posthog.__loaded) posthog.group("organization", organizationId, properties);
}

export function capturePostHog(event: string, properties?: Record<string, string>) {
  if (posthog.__loaded) posthog.capture(event, properties);
}

/** The next person on this browser starts anonymous. */
export function resetPostHog() {
  if (posthog.__loaded) posthog.reset();
}
