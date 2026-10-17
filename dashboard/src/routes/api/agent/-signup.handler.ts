import { Effect } from "effect";
import ipaddr from "ipaddr.js";
import { signUpAnonymous } from "#/modules/identity/anonymous-signup.server";
import { admitWithinWindows, type RateWindow } from "#/modules/machines/waiting/rate-limit.server";
import { publicErrorResponse, RateLimited, Validation } from "#/server/public-error";

export const SIGNUP_WINDOWS: ReadonlyArray<RateWindow> = [
  { seconds: 60 * 60, limit: 5 },
  { seconds: 24 * 60 * 60, limit: 20 },
];

const noStore = { "cache-control": "no-store" };

/** One host rotates freely inside its IPv6 /64, so that prefix is the unit a limit can hold. */
const rateLimitAddress = (clientIp: string) => {
  if (!ipaddr.isValid(clientIp)) return clientIp;
  const address = ipaddr.process(clientIp);
  if (address.range() === "unspecified") return "unknown";
  if (address.kind() === "ipv4") return address.toString();
  return `${ipaddr.IPv6.networkAddressFromCIDR(`${address.toString()}/64`).toString()}/64`;
};

export const handleAgentSignup = Effect.fn("Agent.signup")(function* (clientIp: string) {
  const admission = yield* admitWithinWindows(`agent-signup:${rateLimitAddress(clientIp)}`, SIGNUP_WINDOWS);
  if (!admission.admitted) {
    return publicErrorResponse(
      new RateLimited({ message: "Too many signups from this address. Try again later.", userFacing: true }),
      { headers: { ...noStore, "retry-after": String(admission.retryAfterSeconds) } },
    );
  }
  const signup = yield* signUpAnonymous();
  return Response.json(
    {
      token: signup.token,
      organization: signup.organization,
      claimUrl: signup.claimUrl,
      expiresAt: signup.expiresAt.toISOString(),
    },
    { status: 201, headers: noStore },
  );
});

export const signupMethodNotAllowed = () =>
  publicErrorResponse(
    new Validation({ message: "Sign up with POST.", userFacing: true }),
    { status: 405, headers: { ...noStore, allow: "POST" } },
  );
