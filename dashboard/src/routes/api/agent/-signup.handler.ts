import { Effect } from "effect";
import { signUpAnonymous } from "#/modules/identity/anonymous-signup.server";
import { admitWithinWindows, type RateWindow } from "#/modules/machines/waiting/rate-limit.server";
import { publicErrorResponse, Validation } from "#/server/public-error";

export const SIGNUP_WINDOWS: ReadonlyArray<RateWindow> = [
  { seconds: 60 * 60, limit: 5 },
  { seconds: 24 * 60 * 60, limit: 20 },
];

const noStore = { "cache-control": "no-store" };

export const handleAgentSignup = Effect.fn("Agent.signup")(function* (clientIp: string) {
  const admission = yield* admitWithinWindows(`agent-signup:${clientIp}`, SIGNUP_WINDOWS);
  if (!admission.admitted) {
    return publicErrorResponse(
      new Validation({ message: "Too many signups from this address. Try again later.", userFacing: true }),
      { status: 429, headers: { ...noStore, "retry-after": String(admission.retryAfterSeconds) } },
    );
  }
  const signup = yield* signUpAnonymous();
  return Response.json(
    {
      token: signup.token,
      organization: signup.organization,
      claimUrl: null,
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
