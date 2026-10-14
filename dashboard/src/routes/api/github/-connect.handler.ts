import { Effect } from "effect";
import { Auth } from "#/server/auth.server";
import { completeGithubConnect, startGithubConnect } from "#/modules/github/github-connect.server";
import { Forbidden, NotFound, Unauthorized } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const seeOther = (location: string) => new Response(null, { status: 303, headers: { location, "cache-control": "no-store" } });

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

function refusal(status: number, message: string) {
  const html = `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>GitHub not connected</title>`
    + `<body style="font:15px system-ui,sans-serif;max-width:28rem;margin:20vh auto;padding:0 1.5rem;color:#111">`
    + `<h1 style="font-size:18px;font-weight:600">GitHub not connected</h1><p style="color:#555">${escape(message)}</p>`
    + `<p><a href="/cloud" style="color:#111">Back to Ployz</a></p></body>`;
  return new Response(html, { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } });
}

const signedIn = Effect.fn("GithubConnect.signedIn")(function* (request: Request) {
  const session = yield* (yield* Auth).getSession(request.headers);
  if (session === null) return yield* new Unauthorized();
  return { userId: session.user.id };
});

type Program = Parameters<typeof runAppEffect<string, unknown>>[0];

function respond(request: Request, effect: Program) {
  return runAppEffect(
    effect.pipe(
      Effect.map(seeOther),
      Effect.catchIf(
        (error) => error instanceof Unauthorized || error instanceof Forbidden || error instanceof NotFound,
        (error) => Effect.succeed(error instanceof Unauthorized ? seeOther("/auth")
          : error instanceof Forbidden ? refusal(403, error.message) : refusal(404, "No such Organization.")),
      ),
      Effect.catchCause((cause) =>
        Effect.logError("GitHub connect failed.", cause).pipe(Effect.as(refusal(500, "Something went wrong. Try again."))),
      ),
    ),
    { signal: request.signal },
  );
}

/** `GET /api/github/connect?organization=SLUG[&existing=1]`: off to GitHub to install or authorize the App. */
export function handleGithubConnect(request: Request) {
  const params = new URL(request.url).searchParams;
  return respond(request, Effect.gen(function* () {
    const actor = yield* signedIn(request);
    return yield* startGithubConnect(actor, params.get("organization") ?? "", params.get("existing") === "1");
  }));
}

/** `GET /api/github/callback`: GitHub sends the person back here after installing or authorizing. */
export function handleGithubCallback(request: Request) {
  const params = new URL(request.url).searchParams;
  return respond(request, Effect.gen(function* () {
    return yield* completeGithubConnect(yield* signedIn(request), params);
  }));
}
