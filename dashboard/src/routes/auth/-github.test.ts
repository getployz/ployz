import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import { Effect } from "effect";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as auth from "#/auth/auth.server";
import { AuthenticationUnavailable, type AuthSession } from "#/server/auth.server";
import { handleGithubSignIn } from "./github";

useServiceFreeEffectRunner();

const GITHUB_AUTHORIZE = "https://github.com/login/oauth/authorize?state=abc";
// SAFETY: The route only checks whether a session exists.
const SESSION = {} as AuthSession;

const getSession = vi.spyOn(auth, "getAuthSessionEffect");
const signIn = vi.spyOn(auth, "signInGithubEffect");

describe("GET /auth/github", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    getSession.mockImplementation(() => Effect.succeed(null));
    signIn.mockImplementation(() =>
      Effect.succeed(
        new Response(null, {
          status: 200,
          headers: { location: GITHUB_AUTHORIZE, "set-cookie": "provider-secret=hidden", "content-type": "application/json" },
        }),
      ),
    );
  });

  it("sends a signed-out visitor to GitHub with the safe callback", async () => {
    const response = await handleGithubSignIn(request("/cloud/acme/project?tab=logs"));

    expect(signIn).toHaveBeenCalledWith(expect.any(Headers), "/cloud/acme/project?tab=logs");
    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe(GITHUB_AUTHORIZE);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.has("set-cookie")).toBe(false);
    expect(response.headers.has("content-type")).toBe(false);
  });

  it.each([["//attacker.example"], ["https://attacker.example"], ["/\\attacker.example"], ["/\t/attacker.example"], ["/\n/attacker.example"], [null]])(
    "falls back to /cloud for callback %s",
    async (callbackURL) => {
      await handleGithubSignIn(request(callbackURL));

      expect(signIn).toHaveBeenCalledWith(expect.any(Headers), "/cloud");
    },
  );

  it("sends a signed-in visitor straight to the callback without starting OAuth", async () => {
    getSession.mockImplementation(() => Effect.succeed(SESSION));

    const response = await handleGithubSignIn(request("/cloud/acme"));

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe("/cloud/acme");
    expect(signIn).not.toHaveBeenCalled();
  });

  it("keeps a signed-in visitor on this origin", async () => {
    getSession.mockImplementation(() => Effect.succeed(SESSION));

    const response = await handleGithubSignIn(request("/\\evil.com"));

    expect(response.headers.get("location")).toBe("/cloud");
  });

  it("returns the callback as given, since /.//evil.com normalizes to //evil.com", async () => {
    getSession.mockImplementation(() => Effect.succeed(SESSION));

    const response = await handleGithubSignIn(request("/.//evil.com"));

    expect(response.headers.get("location")).toBe("/.//evil.com");
  });

  it("shows the failed sign-in page when better-auth refuses", async () => {
    signIn.mockImplementation(() => Effect.succeed(new Response("{}", { status: 400 })));

    const response = await handleGithubSignIn(request("/cloud"));

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe("/auth?error=github_unavailable");
  });

  it("shows the failed sign-in page when auth is unavailable", async () => {
    getSession.mockImplementation(() => Effect.fail(new AuthenticationUnavailable({ cause: new Error("down") })));

    const response = await handleGithubSignIn(request("/cloud"));

    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe("/auth?error=github_unavailable");
    expect(signIn).not.toHaveBeenCalled();
  });
});

function request(callbackURL: string | null) {
  const url = new URL("http://localhost/auth/github");
  if (callbackURL !== null) url.searchParams.set("callbackURL", callbackURL);
  return new Request(url);
}
