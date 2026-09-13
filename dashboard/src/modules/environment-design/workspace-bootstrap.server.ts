import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import { session as schemaSession } from "#/modules/identity/tables";
import { ensureOrganizationFreeSubscription } from "#/modules/billing/billing.server";
import { Database } from "#/server/database.server";
import {
  ensurePersonalOrganizationForUser,
  getFirstOrganizationIdForUser,
  getOrganizationSlugById,
  type PersonalOrganizationUser,
} from "./workspace-repository.server";

type SessionRecord = {
  id: string;
  userId: string;
  token?: string | null;
  activeOrganizationId?: string | null;
  activeOrganizationSlug?: string | null;
};

type SessionContext = {
  context: {
    internalAdapter: {
      findUserById(userId: string): Promise<PersonalOrganizationUser | null>;
      updateSession(
        token: string,
        session: {
          activeOrganizationId: string;
          activeOrganizationSlug: string | null;
        },
      ): Promise<SessionRecord | null>;
    };
  };
};

export class WorkspaceAuthHookFailure extends Data.TaggedError(
  "WorkspaceAuthHookFailure",
)<{ readonly cause: unknown }> {}

const setActiveOrganizationForSession = Effect.fn(
  "Workspace.setActiveOrganizationForSession",
)(function* (
  sessionRecord: SessionRecord,
  organizationId: string,
  ctx: SessionContext,
) {
  const organizationSlug = yield* getOrganizationSlugById(organizationId);

  const token = sessionRecord.token;
  if (token) {
    yield* Effect.tryPromise({
      try: () => ctx.context.internalAdapter.updateSession(token, {
        activeOrganizationId: organizationId,
        activeOrganizationSlug: organizationSlug,
      }),
      catch: (cause) => new WorkspaceAuthHookFailure({ cause }),
    });
    return;
  }

  const database = yield* Database;
  yield* database.drizzle
    .update(schemaSession)
    .set({
      activeOrganizationId: organizationId,
      activeOrganizationSlug: organizationSlug,
    })
    .where(eq(schemaSession.id, sessionRecord.id));
});

export const handleUserCreated = Effect.fn("Workspace.handleUserCreated")(
  function* (user: PersonalOrganizationUser) {
    const organizationId = yield* ensurePersonalOrganizationForUser(user);
    yield* ensureOrganizationFreeSubscription({
      organizationId,
      userId: user.id,
    });
  },
);

export const handleSessionCreated = Effect.fn("Workspace.handleSessionCreated")(
function* (
  sessionRecord: SessionRecord,
  ctx: SessionContext | null,
) {
  if (!ctx || sessionRecord.activeOrganizationId) return;
  let organizationId = yield* getFirstOrganizationIdForUser(sessionRecord.userId);
  if (!organizationId) {
    const user = yield* Effect.tryPromise({
      try: () => ctx.context.internalAdapter.findUserById(sessionRecord.userId),
      catch: (cause) => new WorkspaceAuthHookFailure({ cause }),
    });
    if (!user) return;
    organizationId = yield* ensurePersonalOrganizationForUser(user);
  }
  if (!organizationId) return;
  yield* setActiveOrganizationForSession(sessionRecord, organizationId, ctx);
});
