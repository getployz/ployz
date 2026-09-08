import { Effect } from "effect";
import { beforeEach, describe, expect, it, vi } from "vitest";
import * as auth from "#/auth/auth.server";
import { handleGithubSignIn } from "./github";

vi.spyOn(auth, "signInGithubEffect").mockImplementation(
  (_headers, callbackURL) =>
    Effect.succeed(
      new Response(null, {
        status: 200,
        headers: {
          location: callbackURL,
          "set-cookie": "provider-secret=hidden",
          "content-type": "application/json",
        },
      }),
    ),
);

describe("GitHub OAuth route", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("decodes form data and preserves a safe relative callback", async () => {
    const response = await handleGithubSignIn(
      requestWithCallback("/cloud/acme/project"),
    );

    expect(auth.signInGithubEffect).toHaveBeenCalledWith(
      expect.any(Headers),
      "/cloud/acme/project",
    );
    expect(response.status).toBe(303);
    expect(response.headers.get("location")).toBe("/cloud/acme/project");
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(response.headers.has("set-cookie")).toBe(false);
    expect(response.headers.has("content-type")).toBe(false);
  });

  it("falls back instead of accepting a protocol-relative redirect", async () => {
    await handleGithubSignIn(requestWithCallback("//attacker.example"));

    expect(auth.signInGithubEffect).toHaveBeenCalledWith(
      expect.any(Headers),
      "/cloud",
    );
  });

  it("rejects non-string form values at the public boundary", async () => {
    const form = new FormData();
    form.append("callbackURL", new Blob(["/cloud"]), "callback.txt");

    const response = await handleGithubSignIn(
      new Request("http://localhost/api/auth/github", {
        method: "POST",
        body: form,
      }),
    );

    expect(response.status).toBe(422);
    await expect(response.json()).resolves.toEqual({
      _tag: "PublicError",
      code: "VALIDATION_FAILED",
      message: "The request is invalid.",
    });
    expect(auth.signInGithubEffect).not.toHaveBeenCalled();
  });
});

function requestWithCallback(callbackURL: string) {
  const form = new FormData();
  form.set("callbackURL", callbackURL);
  return new Request("http://localhost/api/auth/github", {
    method: "POST",
    body: form,
  });
}
