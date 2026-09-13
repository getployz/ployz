import {
  createRuntimeEventsResponse,
  type RuntimeWatch,
} from "#/modules/runtime/runtime-events.server";
import { publicErrorResponse, Validation } from "#/server/public-error";

export type RuntimeEventsHandlerDeps = {
  openRuntimeWatch: (input: {
    request: Request;
    organizationSlug: string;
  }) => Promise<RuntimeWatch>;
};

export async function handleRuntimeEventsRequest(
  request: Request,
  organizationSlug: string,
  deps: RuntimeEventsHandlerDeps,
) {
  try {
    const watch = await deps.openRuntimeWatch({
      request,
      organizationSlug,
    });
    return createRuntimeEventsResponse({ request, ...watch });
  } catch (cause) {
    return publicErrorResponse(cause);
  }
}

export function runtimeEventsValidationError() {
  return new Validation({
    message: "A valid organization slug is required",
  });
}
