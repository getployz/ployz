import { gzipSync } from "node:zlib";
import { createMemoryHistory } from "@tanstack/react-router";
import type { CaptureResult, Properties } from "posthog-js";
import posthog from "posthog-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { followPostHogNavigation, startPostHog } from "#/modules/analytics/posthog";
import { getRouter } from "#/router";

const router = getRouter();

const config = { key: "phc_test", host: "https://ph.example.test" };
const SECRET_URLS = [
  "https://cloud.example.test/claim/s3cret",
  "https://cloud.example.test/invite/s3cret",
  "https://cloud.example.test/device?user_code=ABCD-EFGH",
  "https://cloud.example.test/Claim/s3cret",
  "https://cloud.example.test/CLAIM/s3cret",
];

const openAt = (url: string) => vi.stubGlobal("window", { location: new URL(url) });

const beforeSend = () => {
  const options = vi.mocked(posthog.init).mock.calls[0]?.[1];
  const fns = [options?.before_send ?? []].flat();
  return (event: CaptureResult) => fns.reduce<CaptureResult | null>((sent, fn) => (sent === null ? null : fn(sent)), event);
};

const event = (properties: Properties, rest: Partial<CaptureResult> = {}): CaptureResult =>
  ({ uuid: "e", event: "$pageview", properties, ...rest });

/**
 * Session replay as posthog-js runs it: stop and start flip `disable_session_recording`, and the recorder only starts
 * once remote config arrives, so replay can be on its way while sessionRecordingStarted() still says false.
 */
const replay = { remoteConfigLoaded: false, started: false };
const syncReplay = () => {
  replay.started = replay.remoteConfigLoaded && !posthog.config.disable_session_recording;
};
const loadRemoteConfig = () => {
  replay.remoteConfigLoaded = true;
  syncReplay();
};

beforeEach(() => {
  replay.remoteConfigLoaded = false;
  replay.started = false;
  vi.spyOn(posthog, "init").mockImplementation((_key, options) => {
    posthog.config.disable_session_recording = options?.disable_session_recording ?? false;
    posthog.__loaded = true;
    syncReplay();
    return posthog;
  });
  vi.spyOn(posthog, "startSessionRecording").mockImplementation(() => {
    posthog.config.disable_session_recording = false;
    syncReplay();
  });
  vi.spyOn(posthog, "stopSessionRecording").mockImplementation(() => {
    posthog.config.disable_session_recording = true;
    syncReplay();
  });
  vi.spyOn(posthog, "sessionRecordingStarted").mockImplementation(() => replay.started);
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  posthog.__loaded = false;
  posthog.config.disable_session_recording = false;
});

describe("startPostHog", () => {
  it.each(SECRET_URLS)("starts nothing on a page whose URL is a secret: %s", (url) => {
    openAt(url);
    startPostHog(config, null, router);
    expect(posthog.init).not.toHaveBeenCalled();
  });

  it("starts on any other page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    expect(posthog.init).toHaveBeenCalledOnce();
  });

  it.each(SECRET_URLS)("sends no event recorded on a secret page reached without a page load: %s", (url) => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const send = beforeSend();
    openAt(url);
    expect(send(event({ $current_url: url }))).toBeNull();
    expect(send(event({ $current_url: "https://cloud.example.test/cloud/acme/~" }, { event: "$snapshot" }))).toBeNull();
  });

  it("strips a secret URL a later page carries as its referrer or previous page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const send = beforeSend();
    const claim = "https://cloud.example.test/claim/s3cret";
    const sent = send(event(
      { $current_url: "https://cloud.example.test/cloud/acme/~", $referrer: claim, $prev_pageview_pathname: "/claim/s3cret", $pathname: "/cloud/acme/~" },
      { $set_once: { $initial_referrer: claim, $initial_current_url: "https://cloud.example.test/cloud/acme/~" } },
    ));
    expect(sent?.properties).toEqual({ $current_url: "https://cloud.example.test/cloud/acme/~", $pathname: "/cloud/acme/~" });
    expect(sent?.$set_once).toEqual({ $initial_current_url: "https://cloud.example.test/cloud/acme/~" });
  });

  it("strips a secret URL whatever its case", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const sent = beforeSend()(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $referrer: "https://x/CLAIM/abc", $prev_pageview_pathname: "/Claim/abc" }));
    expect(sent?.properties).toEqual({ $current_url: "https://cloud.example.test/cloud/acme/~" });
  });

  it("drops a replay batch whose snapshot carries a secret URL", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const meta = { type: 4, timestamp: 1, data: { href: "https://cloud.example.test/claim/s3cret", width: 1280, height: 720 } };
    const link = { type: 2, timestamp: 2, data: { node: { childNodes: [{ tagName: "a", attributes: { href: "/invite/s3cret" } }] } } };
    const send = beforeSend();
    for (const snapshot of [meta, link]) {
      expect(send(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $snapshot_data: [snapshot] }, { event: "$snapshot" }))).toBeNull();
    }
  });

  it("keeps a replay batch from an ordinary page, even one whose text names a secret page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const snapshot = { type: 2, timestamp: 1, data: { href: "https://cloud.example.test/cloud/acme/~", node: { childNodes: [{ textContent: "Invite" }, { textContent: "device" }] } } };
    const properties = { $current_url: "https://cloud.example.test/cloud/acme/~", $snapshot_data: [snapshot] };
    expect(beforeSend()(event(properties, { event: "$snapshot" }))?.properties).toEqual(properties);
  });

  it("drops a heatmap batch keyed by a secret page's URL, and keeps an ordinary one", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const heatmap = (url: string) =>
      event({ $current_url: "https://cloud.example.test/cloud/acme/~", $heatmap_data: { [url]: [{ x: 1, y: 2, target_fixed: false, type: "click" }] } }, { event: "$$heatmap" });
    expect(beforeSend()(heatmap("https://cloud.example.test/claim/s3cret"))).toBeNull();
    expect(beforeSend()(heatmap("https://cloud.example.test/cloud/acme/~"))).not.toBeNull();
  });
});

/** Whether the app's router renders a secret page for this path, by route id rather than by the flag the code reads. */
const SECRET_ROUTES = ["/claim/$secret", "/invite/$token", "/device"];
const routerOpensSecret = (path: string) =>
  SECRET_ROUTES.includes(router.matchRoutes(router.parseLocation(createMemoryHistory({ initialEntries: [path] }).location)).at(-1)?.routeId ?? "");

describe("a secret page is whatever the router renders as one", () => {
  const PROBES: [string, boolean][] = [
    ["/%63laim/s3cret", true],
    ["/cl%61im/s3cret", true],
    ["//claim/s3cret", true],
    ["/claim/s3cret/", true],
    ["/CLAIM/s3cret", true],
    ["/claim/s3cret?from=cli", true],
    ["/%69nvite/t0ken", true],
    ["/d%65vice", true],
    ["/claim%2Fs3cret", false],
    ["/claim", false],
    ["/cloud/acme/~", false],
  ];

  it.each(PROBES)("the router agrees on %s", (path, secret) => {
    expect(routerOpensSecret(path)).toBe(secret);
  });

  it.each(PROBES)("starts on %s only when it is not a secret page", (path, secret) => {
    openAt(`https://cloud.example.test${path}`);
    startPostHog(config, null, router);
    expect(vi.mocked(posthog.init).mock.calls.length).toBe(secret ? 0 : 1);
  });

  it.each(PROBES)("drops a pageview of %s, and strips it as a referrer, only when it is a secret page", (path, secret) => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    const send = beforeSend();
    const url = `https://cloud.example.test${path}`;
    expect(send(event({ $current_url: url })) === null).toBe(secret);
    const later = send(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $referrer: url, $prev_pageview_pathname: path }));
    expect(later?.properties["$referrer"] === undefined).toBe(secret);
    expect(later?.properties["$prev_pageview_pathname"] === undefined).toBe(secret);
  });

  it.each(PROBES)("stops the replay on a client navigation to %s only when it is a secret page", (path, secret) => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null, router);
    loadRemoteConfig();
    followPostHogNavigation(path);
    expect(posthog.sessionRecordingStarted()).toBe(!secret);
  });
});

describe("followPostHogNavigation", () => {
  const startOn = (url: string) => {
    openAt(url);
    startPostHog(config, null, router);
  };

  it("stops the replay before a client navigation reaches a secret page, and keeps it off for the rest of the page load", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    expect(posthog.sessionRecordingStarted()).toBe(true);
    followPostHogNavigation("/claim/s3cret");
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(false);
  });

  it("sends nothing from the claim page's DOM when Back leaves it while that DOM is still mounted", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    const send = beforeSend();
    const sent: (CaptureResult | null)[] = [];
    // A restarted recorder takes a full snapshot of whatever is on screen, compressed the way posthog-js sends it.
    vi.mocked(posthog.startSessionRecording).mockImplementation(() => {
      posthog.config.disable_session_recording = false;
      syncReplay();
      const dom = { node: { childNodes: [{ tagName: "a", attributes: { href: "/claim/s3cret" } }] } };
      const data = gzipSync(Buffer.from(JSON.stringify(dom))).toString("latin1");
      sent.push(send(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $snapshot_data: [{ type: 2, data, cv: "2024-10", timestamp: 1 }] }, { event: "$snapshot" })));
    });
    followPostHogNavigation("/claim/s3cret");
    openAt("https://cloud.example.test/cloud/acme/~");
    followPostHogNavigation("/cloud/acme/~");
    expect(sent.filter((batch) => batch !== null)).toEqual([]);
    expect(posthog.sessionRecordingStarted()).toBe(false);
  });

  it("stops a replay that has not started recording yet, so it never starts on the secret page", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/claim/s3cret");
    loadRemoteConfig();
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(false);
  });

  it("stops the replay on a secret path whatever its case", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    followPostHogNavigation("/Claim/abc");
    expect(posthog.sessionRecordingStarted()).toBe(false);
  });

  it("does not start a replay that was already off when it leaves a secret page", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    posthog.stopSessionRecording();
    followPostHogNavigation("/claim/s3cret");
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(false);
  });

  it("leaves the replay alone between pages without secrets", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    followPostHogNavigation("/cloud/acme/~");
    followPostHogNavigation("/cloud/acme/settings");
    expect(posthog.sessionRecordingStarted()).toBe(true);
    expect(posthog.stopSessionRecording).not.toHaveBeenCalled();
    expect(posthog.startSessionRecording).not.toHaveBeenCalled();
  });
});
