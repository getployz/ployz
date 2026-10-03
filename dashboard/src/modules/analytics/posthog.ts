import { createIsomorphicFn } from "@tanstack/react-start";
import posthog from "posthog-js";
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
 * Starts PostHog once per page load. Sign-in arrives through a full page load, so identifying here covers it.
 * Replays show inputs: anything secret carries `ph-no-capture` (see CODING_STANDARDS.md).
 */
export function startPostHog(config: PostHogBrowserConfig, session: AuthSession | null) {
  if (config === null) return;
  posthog.init(config.key, {
    api_host: config.host,
    // ponytail: the PostHog app's own origin, for links and the toolbar while api_host is the managed proxy.
    ui_host: "https://us.posthog.com",
    defaults: "2026-08-30",
    capture_exceptions: true,
    session_recording: { maskAllInputs: false, maskInputOptions: { password: true } },
  });
  if (session !== null) {
    posthog.identify(session.user.id, { email: session.user.email, name: session.user.name });
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
