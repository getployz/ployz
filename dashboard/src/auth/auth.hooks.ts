import type { AuthSession } from "#/auth/auth";
import { announceSignOut, authClient } from "#/auth/auth-client";
import { Data, Result } from "effect";
import { useHydrated, useMatch, useRouter } from "@tanstack/react-router";

type AuthSessionValue = {
  data: AuthSession | null;
  isPending: false;
  refetch: ReturnType<typeof authClient.useSession>["refetch"];
};

class SignOutError extends Data.TaggedError("SignOutError")<{
  readonly cause: unknown;
}> {}

function useAuthSession(): AuthSessionValue {
  const serverSession = useMatch({ from: "__root__", select: (match) => match.loaderData?.session ?? null });
  const hydrated = useHydrated();
  const state = authClient.useSession();
  return {
    data: hydrated && !state.isPending ? state.data : serverSession,
    isPending: false,
    refetch: state.refetch,
  };
}

export function useAuth() {
  return useAuthSession().data;
}

export function useSignOut() {
  const router = useRouter();

  return async () => {
    try {
        const response = await authClient.signOut();

        if (response.error) {
          throw response.error;
        }
        announceSignOut();

        await router.navigate({ to: "/", reloadDocument: true });
        return Result.succeed(undefined);
    } catch (cause) {
      return Result.fail(new SignOutError({ cause }));
    }
  };
}
