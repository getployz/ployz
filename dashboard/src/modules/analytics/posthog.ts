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

const isSecretPath = (pathname: string) => SECRET_PATHS.some((path) => pathname === path || pathname.startsWith(`${path}/`));

const isSecretUrl = (value: string) => URL.canParse(value, window.location.origin) &&
  isSecretPath(new URL(value, window.location.origin).pathname);

const withoutSecretUrls = (properties: Properties | undefined) =>
  properties === undefined ? undefined : Object.fromEntries(Object.entries(properties).filter(([, value]) => !isSecretUrl(String(value))));

function dropSecretUrls(event: CaptureResult | null): CaptureResult | null {
  if (event === null || isSecretPath(window.location.pathname) || isSecretUrl(String(event.properties["$current_url"]))) return null;
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

let pausedOnSecretPage = false;

/** Called before each client navigation: the replay pauses for a secret page and resumes once it is left. */
export function followPostHogNavigation(pathname: string) {
  if (!posthog.__loaded) return;
  if (isSecretPath(pathname)) {
    if (!pausedOnSecretPage && posthog.sessionRecordingStarted()) {
      pausedOnSecretPage = true;
      posthog.stopSessionRecording();
    }
  } else if (pausedOnSecretPage) {
    pausedOnSecretPage = false;
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
