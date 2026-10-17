import type { CaptureResult, Properties } from "posthog-js";
import posthog from "posthog-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { followPostHogNavigation, startPostHog } from "#/modules/analytics/posthog";


const config = { key: "phc_test", host: "https://ph.example.test" };
const SECRET_URLS = [
  "https://cloud.example.test/claim/s3cret",
  "https://cloud.example.test/invite/s3cret",
  "https://cloud.example.test/device?user_code=ABCD-EFGH",
];

const openAt = (url: string) => vi.stubGlobal("window", { location: new URL(url) });

const beforeSend = () => {
  const options = vi.mocked(posthog.init).mock.calls[0]?.[1];
  const fns = [options?.before_send ?? []].flat();
  return (event: CaptureResult) => fns.reduce<CaptureResult | null>((sent, fn) => (sent === null ? null : fn(sent)), event);
};

const event = (properties: Properties, rest: Partial<CaptureResult> = {}): CaptureResult =>
  ({ uuid: "e", event: "$pageview", properties, ...rest });

beforeEach(() => {
  vi.spyOn(posthog, "init").mockReturnValue(posthog);
  vi.spyOn(posthog, "stopSessionRecording").mockReturnValue(undefined);
  vi.spyOn(posthog, "startSessionRecording").mockReturnValue(undefined);
  vi.spyOn(posthog, "sessionRecordingStarted").mockReturnValue(true);
  posthog.__loaded = true;
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  posthog.__loaded = false;
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
});

describe("followPostHogNavigation", () => {
  it("stops the replay before a client navigation reaches a secret page, and starts it again after", () => {
    followPostHogNavigation("/claim/s3cret");
    expect(posthog.stopSessionRecording).toHaveBeenCalledOnce();
    expect(posthog.startSessionRecording).not.toHaveBeenCalled();
    followPostHogNavigation("/cloud/acme/~");
    expect(posthog.startSessionRecording).toHaveBeenCalledOnce();
  });

  it("leaves the replay alone between pages without secrets", () => {
    followPostHogNavigation("/cloud/acme/~");
    followPostHogNavigation("/cloud/acme/settings");
    expect(posthog.stopSessionRecording).not.toHaveBeenCalled();
    expect(posthog.startSessionRecording).not.toHaveBeenCalled();
  });
});
