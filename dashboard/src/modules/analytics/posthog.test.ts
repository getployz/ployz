import type { CaptureResult, Properties } from "posthog-js";
import posthog from "posthog-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { followPostHogNavigation, startPostHog } from "#/modules/analytics/posthog";


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
  // The pause on a secret page is module state; leave every test on an ordinary page.
  followPostHogNavigation("/cloud/acme/~");
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  posthog.__loaded = false;
  posthog.config.disable_session_recording = false;
});

describe("startPostHog", () => {
  it.each(SECRET_URLS)("starts nothing on a page whose URL is a secret: %s", (url) => {
    openAt(url);
    startPostHog(config, null);
    expect(posthog.init).not.toHaveBeenCalled();
  });

  it("starts on any other page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null);
    expect(posthog.init).toHaveBeenCalledOnce();
  });

  it.each(SECRET_URLS)("sends no event recorded on a secret page reached without a page load: %s", (url) => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null);
    const send = beforeSend();
    openAt(url);
    expect(send(event({ $current_url: url }))).toBeNull();
    expect(send(event({ $current_url: "https://cloud.example.test/cloud/acme/~" }, { event: "$snapshot" }))).toBeNull();
  });

  it("strips a secret URL a later page carries as its referrer or previous page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null);
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
    startPostHog(config, null);
    const sent = beforeSend()(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $referrer: "https://x/CLAIM/abc", $prev_pageview_pathname: "/Claim/abc" }));
    expect(sent?.properties).toEqual({ $current_url: "https://cloud.example.test/cloud/acme/~" });
  });

  it("drops a replay batch whose snapshot carries a secret URL", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null);
    const meta = { type: 4, timestamp: 1, data: { href: "https://cloud.example.test/claim/s3cret", width: 1280, height: 720 } };
    const link = { type: 2, timestamp: 2, data: { node: { childNodes: [{ tagName: "a", attributes: { href: "/invite/s3cret" } }] } } };
    const send = beforeSend();
    for (const snapshot of [meta, link]) {
      expect(send(event({ $current_url: "https://cloud.example.test/cloud/acme/~", $snapshot_data: [snapshot] }, { event: "$snapshot" }))).toBeNull();
    }
  });

  it("keeps a replay batch from an ordinary page, even one whose text names a secret page", () => {
    openAt("https://cloud.example.test/cloud/acme/~");
    startPostHog(config, null);
    const snapshot = { type: 2, timestamp: 1, data: { href: "https://cloud.example.test/cloud/acme/~", node: { childNodes: [{ textContent: "Invite" }, { textContent: "device" }] } } };
    const properties = { $current_url: "https://cloud.example.test/cloud/acme/~", $snapshot_data: [snapshot] };
    expect(beforeSend()(event(properties, { event: "$snapshot" }))?.properties).toEqual(properties);
  });
});

describe("followPostHogNavigation", () => {
  const startOn = (url: string) => {
    openAt(url);
    startPostHog(config, null);
  };

  it("stops the replay before a client navigation reaches a secret page, and starts it again after", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    loadRemoteConfig();
    expect(posthog.sessionRecordingStarted()).toBe(true);
    followPostHogNavigation("/claim/s3cret");
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(true);
  });

  it("stops a replay that has not started recording yet, so it never starts on the secret page", () => {
    startOn("https://cloud.example.test/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/claim/s3cret");
    loadRemoteConfig();
    expect(posthog.sessionRecordingStarted()).toBe(false);
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.sessionRecordingStarted()).toBe(true);
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
