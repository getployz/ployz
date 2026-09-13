import { Route as RootRoute } from "#/routes/__root";
import type { AuthSession } from "#/auth/auth";
import { authClient } from "#/auth/auth-client";
import { Data, Result } from "effect";
import { useHydrated, useRouter } from "@tanstack/react-router";

type AuthSessionValue = {
  data: AuthSession | null;
  isPending: false;
};

class SignOutError extends Data.TaggedError("SignOutError")<{
  readonly cause: unknown;
}> {}

function useAuthSession(): AuthSessionValue {
  const serverSession = RootRoute.useLoaderData().session ?? null;
  const hydrated = useHydrated();
  const state = authClient.useSession();
  return {
    data: hydrated ? state.data : serverSession,
    isPending: false,
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

        await router.navigate({ to: "/", reloadDocument: true });
        return Result.succeed(undefined);
    } catch (cause) {
      return Result.fail(new SignOutError({ cause }));
    }
  };
}

export function getSignOutErrorMessage(_error: SignOutError) {
  return "Couldn't sign out. Try again";
}
