import { describe, expect, it } from "vitest";
import { getBetterAuthUrlConfig } from "#/auth/trusted-origins";

describe("getBetterAuthUrlConfig", () => {
  it("preserves wildcard trusted origins", () => {
    const config = getBetterAuthUrlConfig(
      "https://app.example.com",
      "https://*.vercel.app,https://preview.example.com",
    );

    expect(config.trustedOrigins).toEqual([
      "https://app.example.com",
      "https://*.vercel.app",
      "https://preview.example.com",
    ]);
    expect(config.allowedHosts).toEqual([
      "app.example.com",
      "*.vercel.app",
      "preview.example.com",
    ]);
  });

  it("normalizes wildcard hosts without a scheme", () => {
    const config = getBetterAuthUrlConfig(
      "https://app.example.com",
      "*.preview.example.com",
    );

    expect(config.trustedOrigins).toContain("https://*.preview.example.com");
    expect(config.allowedHosts).toContain("*.preview.example.com");
  });

  it("trusts arbitrary localhost ports when the app url is localhost", () => {
    const config = getBetterAuthUrlConfig("http://localhost:3000");

    expect(config.trustedOrigins).toContain("http://localhost:*");
    expect(config.trustedOrigins).toContain("http://127.0.0.1:*");
    expect(config.allowedHosts).toContain("localhost:*");
    expect(config.allowedHosts).toContain("127.0.0.1:*");
  });

  it("drops origins that cannot parse as URLs", () => {
    const config = getBetterAuthUrlConfig(
      "https://app.example.com",
      "https://ok.example.com, not a host, https://also.example.com",
    );

    expect(config.trustedOrigins).toEqual([
      "https://app.example.com",
      "https://ok.example.com",
      "https://also.example.com",
    ]);
    expect(config.allowedHosts).toContain("ok.example.com");
    expect(config.allowedHosts).toContain("also.example.com");
    expect(config.allowedHosts).toContain("not a host");
  });

  it("can trust localhost for development with a remote app url", () => {
    const config = getBetterAuthUrlConfig(
      "https://dev.example.com",
      undefined,
      { trustLocalhost: true },
    );

    expect(config.trustedOrigins).toContain("http://localhost:*");
    expect(config.trustedOrigins).toContain("http://127.0.0.1:*");
    expect(config.allowedHosts).toContain("localhost:*");
    expect(config.allowedHosts).toContain("127.0.0.1:*");
  });
});
