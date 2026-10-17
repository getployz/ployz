import { Effect, Option, Schema } from "effect";
import { Auth } from "#/server/auth.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { completeGithubConnect, startGithubConnect } from "#/modules/github/github-connect.server";
import { Forbidden, NotFound, Unauthorized } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const seeOther = (location: string) => new Response(null, { status: 303, headers: { location, "cache-control": "no-store" } });

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function refusal(status: number, message: string, link = { href: "/cloud", label: "Back to Ployz" }) {
  const html = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>GitHub not connected</title>`
    + `<body style="font:15px system-ui,sans-serif;max-width:28rem;margin:20vh auto;padding:0 1.5rem;color:#111">`
    + `<h1 style="font-size:18px;font-weight:600">GitHub not connected</h1><p style="color:#555">${escape(message)}</p>`
    + `<p><a href="${escape(link.href)}" style="color:#111">${escape(link.label)}</a></p></body>`;
  return new Response(html, { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } });
}

const signedIn = Effect.fn("GithubConnect.signedIn")(function* (request: Request) {
  const session = yield* (yield* Auth).getSession(request.headers);
  if (session === null) return yield* new Unauthorized();
  return { userId: session.user.id };
});

type Program = Parameters<typeof runAppEffect<string, unknown>>[0];

const ClaimDetails = Schema.Struct({ claim_url: Schema.String });

const claimLink = (error: StoreRefused) => Schema.decodeUnknownOption(ClaimDetails)(error.details).pipe(
  Option.map(({ claim_url }) => ({ href: claim_url, label: "Claim this Organization" })),
  Option.getOrUndefined,
);

const answer = <R>(effect: Effect.Effect<string, unknown, R>) => effect.pipe(
  Effect.map(seeOther),
  Effect.catchIf(
    (error) => error instanceof Unauthorized || error instanceof Forbidden || error instanceof NotFound || error instanceof StoreRefused,
    (error) => Effect.succeed(error instanceof Unauthorized ? seeOther("/auth")
      : error instanceof StoreRefused ? refusal(403, error.message, claimLink(error))
      : error instanceof Forbidden ? refusal(403, error.message) : refusal(404, "No such Organization.")),
  ),
  Effect.catchCause((cause) =>
    Effect.logError("GitHub connect failed.", cause).pipe(Effect.as(refusal(500, "Something went wrong. Try again."))),
  ),
);

function respond(request: Request, effect: Program) {
  return runAppEffect(answer(effect), { signal: request.signal });
}

export const githubConnectResponse = (request: Request) => {
  const params = new URL(request.url).searchParams;
  return answer(Effect.gen(function* () {
    const actor = yield* signedIn(request);
    return yield* startGithubConnect(actor, params.get("organization") ?? "", params.get("existing") === "1");
  }));
};

export function handleGithubConnect(request: Request) {
  return runAppEffect(githubConnectResponse(request), { signal: request.signal });
}

export function handleGithubCallback(request: Request) {
  const params = new URL(request.url).searchParams;
  return respond(request, Effect.gen(function* () {
    return yield* completeGithubConnect(yield* signedIn(request), params);
  }));
}
