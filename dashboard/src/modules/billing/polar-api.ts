import { createPolar, createPolarCore } from "@polar-sh/sdk/2026-10";
import { Redacted } from "effect";

// Every Polar SDK import goes through this file, so Ployz Cloud speaks one Polar API version.
// The SDK entry sends it as `Polar-Version` on every request; the Polar webhook endpoints must
// be set to the same version. @polar-sh/better-auth is built on this entry too: move them together.
export type { Polar, PolarCore, webhooks } from "@polar-sh/sdk/2026-10";
export const POLAR_API_VERSION = "2026-10";

type PolarAccess = {
  readonly accessToken: Redacted.Redacted<string>;
  readonly server: "production" | "sandbox";
};

function options(access: PolarAccess) {
  return {
    accessToken: Redacted.value(access.accessToken),
    environment: access.server,
  };
}

/** The full service client, for our own billing calls. */
export const makePolarClient = (access: PolarAccess) =>
  createPolar(options(access));

/** The bare client @polar-sh/better-auth takes. */
export const makePolarCore = (access: PolarAccess) =>
  createPolarCore(options(access));
