import { useReducer } from "react";
import { PlusIcon } from "lucide-react";
import { ConfirmableInput } from "#/components/stageable/confirmable-input";
import { Button } from "#/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { SERVICE_DEPLOYMENT_DIFF_PATHS } from "#/modules/services/service-deployment-diff/fields";
import { Result, Schema } from "effect";
import {
  serviceHealthcheckPathSchema,
  serviceHealthcheckTimeoutSecondsSchema,
  type ServiceHealthcheck,
} from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

const DEFAULT_TIMEOUT_SECONDS = 300;

type HealthcheckDraftState = {
  draftPath: string | null;
  pathError: string | null;
  draftTimeout: string;
  timeoutError: string | null;
};

type HealthcheckDraftAction =
  | { type: "patch"; patch: Partial<HealthcheckDraftState> }
  | {
      type: "reset";
      draftPath: string | null;
      draftTimeout: string;
    };

function healthcheckDraftReducer(
  state: HealthcheckDraftState,
  action: HealthcheckDraftAction,
): HealthcheckDraftState {
  switch (action.type) {
    case "patch":
      return { ...state, ...action.patch };
    case "reset":
      return {
        draftPath: action.draftPath,
        draftTimeout: action.draftTimeout,
        pathError: null,
        timeoutError: null,
      };
  }
}

export function ServiceHealthcheckSection({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service } = state;
  const healthcheckKey = JSON.stringify(service.healthcheck);

  return (
    <ServiceHealthcheckEditor
      key={`${service.id}:${healthcheckKey}`}
      state={state}
    />
  );
}

function ServiceHealthcheckEditor({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const { service, collection, diff } = state;
  const pathDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.healthcheckPath);
  const timeoutDiff = diff.field(SERVICE_DEPLOYMENT_DIFF_PATHS.healthcheckTimeout);
  const healthcheck = service.healthcheck;

  const baselinePath = healthcheck.type === "http" ? healthcheck.path : null;
  const baselineTimeout =
    healthcheck.type === "http"
      ? healthcheck.timeoutSeconds
      : DEFAULT_TIMEOUT_SECONDS;

  const [draft, dispatchDraft] = useReducer(healthcheckDraftReducer, {
    draftPath: baselinePath,
    pathError: null,
    draftTimeout: String(baselineTimeout),
    timeoutError: null,
  });

  const isOpen = draft.draftPath !== null;
  const isPathDirty = isOpen
    ? healthcheck.type === "http"
      ? draft.draftPath !== healthcheck.path
      : draft.draftPath !== ""
    : false;
  const isTimeoutDirty =
    healthcheck.type === "http" &&
    draft.draftTimeout !== String(healthcheck.timeoutSeconds);

  function commit(next: ServiceHealthcheck) {
    return collection.update(service.id, (draft) => {
      draft.healthcheck = next;
    });
  }

  function confirmPath() {
    if (draft.draftPath === null) {
      return;
    }

    const trimmed = draft.draftPath.trim();

    // Closing without ever committing
    if (trimmed.length === 0 && healthcheck.type === "none") {
      dispatchDraft({ type: "patch", patch: { draftPath: null } });
      return;
    }

    let next: ServiceHealthcheck;

    if (trimmed.length === 0) {
      next = { type: "none" };
    } else {
      const result = Schema.decodeUnknownResult(serviceHealthcheckPathSchema)(
        trimmed,
        strictParseOptions,
      );

      if (Result.isFailure(result)) {
        dispatchDraft({
          type: "patch",
          patch: {
            pathError:
              result.failure instanceof Error
                ? result.failure.message
                : "Invalid value",
          },
        });
        return;
      }

      next = {
        type: "http",
        path: result.success,
        timeoutSeconds:
          healthcheck.type === "http"
            ? healthcheck.timeoutSeconds
            : DEFAULT_TIMEOUT_SECONDS,
      };
    }

    // Optimistic: a failed save rolls back and toasts; restore the path draft too.
    void commit(next).isPersisted.promise.catch(() => {
      dispatchDraft({ type: "patch", patch: { draftPath: baselinePath } });
    });
    dispatchDraft(next.type === "none"
      ? { type: "reset", draftPath: null, draftTimeout: String(DEFAULT_TIMEOUT_SECONDS) }
      : { type: "reset", draftPath: next.path, draftTimeout: String(next.timeoutSeconds) });
  }

  function cancelPath() {
    dispatchDraft({
      type: "patch",
      patch: { draftPath: baselinePath, pathError: null },
    });
  }

  function confirmTimeout() {
    if (healthcheck.type !== "http") {
      return;
    }

    const parsed = Number(draft.draftTimeout);
    const result = Schema.decodeUnknownResult(
      serviceHealthcheckTimeoutSecondsSchema,
    )(parsed, strictParseOptions);

    if (Result.isFailure(result)) {
      dispatchDraft({
        type: "patch",
        patch: {
          timeoutError:
            result.failure instanceof Error
              ? result.failure.message
              : "Invalid value",
        },
      });
      return;
    }

    // Optimistic: a failed save rolls back and toasts; restore the timeout draft too.
    const previousTimeout = String(healthcheck.timeoutSeconds);
    void commit({
      type: "http",
      path: healthcheck.path,
      timeoutSeconds: result.success,
    }).isPersisted.promise.catch(() => {
      dispatchDraft({ type: "patch", patch: { draftTimeout: previousTimeout } });
    });
    dispatchDraft({ type: "patch", patch: { draftTimeout: String(result.success), timeoutError: null } });
  }

  function cancelTimeout() {
    if (healthcheck.type !== "http") {
      return;
    }
    dispatchDraft({
      type: "patch",
      patch: {
        draftTimeout: String(healthcheck.timeoutSeconds),
        timeoutError: null,
      },
    });
  }

  return (
    <FieldGroup>
      <Field>
        <FieldLabel>Healthcheck Path</FieldLabel>
        <FieldDescription>
          Endpoint to be called before a deploy completes to ensure the new
          deployment is live.
        </FieldDescription>
        {isOpen ? (
          <ConfirmableInput
            aria-label="Healthcheck path"
            aria-invalid={draft.pathError ? true : undefined}
            error={draft.pathError}
            isChanged={pathDiff.changed}
            isDirty={isPathDirty}
            placeholder="/up"
            value={draft.draftPath ?? ""}
            onValueChange={(next) => {
              dispatchDraft({
                type: "patch",
                patch: { draftPath: next, pathError: null },
              });
            }}
            onCancel={cancelPath}
            onConfirm={() => {
              confirmPath();
            }}
          />
        ) : (
          <Button
            type="button"
            variant="outline"
            data-changed={pathDiff.changed || undefined}
            onClick={() => {
              dispatchDraft({ type: "patch", patch: { draftPath: "" } });
            }}
          >
            <PlusIcon data-icon="inline-start" />
            Healthcheck Path
          </Button>
        )}
      </Field>

      {isOpen ? (
        <Field>
          <FieldLabel>Healthcheck Timeout</FieldLabel>
          <FieldDescription>
            Number of seconds we will wait for the healthcheck to complete.
          </FieldDescription>
          <ConfirmableInput
            aria-label="Healthcheck timeout"
            aria-invalid={draft.timeoutError ? true : undefined}
            disabled={healthcheck.type !== "http"}
            error={draft.timeoutError}
            inputMode="numeric"
            isChanged={timeoutDiff.changed}
            isDirty={isTimeoutDirty}
            placeholder="300"
            value={healthcheck.type === "http" ? draft.draftTimeout : ""}
            onValueChange={(next) => {
              dispatchDraft({
                type: "patch",
                patch: { draftTimeout: next, timeoutError: null },
              });
            }}
            onCancel={cancelTimeout}
            onConfirm={() => {
              confirmTimeout();
            }}
          />
        </Field>
      ) : null}
    </FieldGroup>
  );
}
