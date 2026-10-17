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
 * Pages whose URL is itself a secret: an invite, a claim link, a device code. PostHog records every URL, so nothing on
 * them reaches it, and no later event carries their URL as a referrer or previous page.
 */
const SECRET_PATHS = ["/invite", "/claim", "/device"];

const isSecretPath = (pathname: string) => {
  const path = pathname.toLowerCase();
  return SECRET_PATHS.some((secret) => path === secret || path.startsWith(`${secret}/`));
};

/** An absolute URL or a rooted path; any other text, like a button's "Invite", is not a URL. */
const isSecretUrl = (value: string) => (value.startsWith("/") || URL.canParse(value)) &&
  URL.canParse(value, window.location.origin) && isSecretPath(new URL(value, window.location.origin).pathname);

const JSON_STRING = /"(?:[^"\\]|\\.)*"/g;

/** Checks every string in a replay batch, however deep: rrweb nests URLs in meta events and DOM snapshots. */
const carriesSecretUrl = (snapshotData: Properties[string]) =>
  JSON.stringify(snapshotData ?? null).match(JSON_STRING)?.some((literal: string) => isSecretUrl(String(JSON.parse(literal)))) ?? false;

const withoutSecretUrls = (properties: Properties | undefined) =>
  properties === undefined ? undefined : Object.fromEntries(Object.entries(properties).filter(([, value]) => !isSecretUrl(String(value))));

/** A replay batch naming a secret page holds that page's DOM too, so the whole batch goes, not just the URL. */
function dropSecretUrls(event: CaptureResult | null): CaptureResult | null {
  if (event === null || isSecretPath(window.location.pathname) || isSecretUrl(String(event.properties["$current_url"])) ||
    carriesSecretUrl(event.properties["$snapshot_data"])) return null;
  return {
    ...event,
    properties: withoutSecretUrls(event.properties) ?? {},
    $set: withoutSecretUrls(event.$set),
    $set_once: withoutSecretUrls(event.$set_once),
  };
}

/**
 * Starts PostHog once per page load. Sign-in arrives through a full page load, so identifying here covers it.
 * Replays show inputs: anything secret carries `ph-no-capture` (see CODING_STANDARDS.md).
 */
export function startPostHog(config: PostHogBrowserConfig, session: AuthSession | null) {
  if (config === null || isSecretPath(window.location.pathname)) return;
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

let resumeReplayAfterSecretPage = false;

/**
 * Called before each client navigation: the replay stops for a secret page and resumes once it is left, if it was on.
 * It stops even when not yet recording, since a replay waiting on remote config would otherwise start on the secret page.
 */
export function followPostHogNavigation(pathname: string) {
  if (!posthog.__loaded) return;
  if (isSecretPath(pathname)) {
    resumeReplayAfterSecretPage ||= !posthog.config.disable_session_recording;
    posthog.stopSessionRecording();
  } else if (resumeReplayAfterSecretPage) {
    resumeReplayAfterSecretPage = false;
    posthog.startSessionRecording();
  }
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
