/**
 * Ployz Cloud no longer takes Fleet Tokens. A host still running the check-in loop of an older `ployz` gets this stop,
 * which ends its loop and disables its `ployz-checkin` unit, with the words it logs on how to add the Server instead.
 * Every caller gets the same answer, whatever it sends: the answer admits nothing and names nothing of any Organization.
 */
export const CHECKIN_ENDED = "Ployz Cloud no longer adds Servers from a Fleet Token, so this check-in stopped. To add this "
  + "server, run `ployz server add --command` where you're signed in, or choose Add server on the Servers page in Ployz "
  + "Cloud, and run the line it prints on this server.";

export function handleCheckin() {
  return Response.json(
    { kind: "stop", reason: "token_revoked", message: CHECKIN_ENDED },
    { headers: { "cache-control": "no-store" } },
  );
}
