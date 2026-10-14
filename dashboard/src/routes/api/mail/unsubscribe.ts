import { createFileRoute } from "@tanstack/react-router";
import { handleUnsubscribeRequest } from "#/routes/api/mail/-unsubscribe.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const handle = ({ request }: { request: Request }) =>
  runAppEffect(handleUnsubscribeRequest(request), { signal: request.signal }).catch(publicErrorResponse);

export const Route = createFileRoute("/api/mail/unsubscribe")({
  server: { handlers: { GET: handle, POST: handle } },
});
