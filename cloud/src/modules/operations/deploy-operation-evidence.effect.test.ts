import { assert, it } from "@effect/vitest";
import { Effect } from "effect";
import {
  decodeRuntimePhaseEvidence,
  type RuntimePhaseEvidenceInput,
} from "./deploy-operation-evidence";

const phaseFinished = {
  eventType: "deploy_phase_finished",
  payload: {
    operationId: "deploy-1",
    phase: 0,
    outcome: "partial",
    services: [
      { serviceId: "api", result: "completed" },
      {
        serviceId: "worker",
        result: "failed",
        failure: { kind: "healthcheck_failed" },
      },
    ],
  },
} as const;

it.effect("strictly decodes persisted runtime phase evidence", () =>
  Effect.gen(function* () {
    const decoded = yield* decodeRuntimePhaseEvidence(phaseFinished);

    assert.deepStrictEqual(decoded, phaseFinished);
  }),
);

it.effect("rejects excess and non-finite persisted phase fields", () =>
  Effect.gen(function* () {
    const withExcess = {
      ...phaseFinished,
      internal_debug: "not evidence",
    };
    const excessInput: RuntimePhaseEvidenceInput = withExcess;
    const excess = yield* decodeRuntimePhaseEvidence(excessInput).pipe(
      Effect.flip,
    );
    const nonFinite = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: { ...phaseFinished.payload, phase: Number.NaN },
    }).pipe(Effect.flip);

    assert.strictEqual(excess._tag, "RuntimePhaseEvidenceInvalid");
    assert.strictEqual(nonFinite._tag, "RuntimePhaseEvidenceInvalid");
  }),
);

it.effect("rejects empty persisted phase outcomes", () =>
  Effect.gen(function* () {
    const emptyOutcome = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: { ...phaseFinished.payload, outcome: "" },
    }).pipe(Effect.flip);

    assert.strictEqual(emptyOutcome._tag, "RuntimePhaseEvidenceInvalid");
  }),
);

it.effect("rejects empty ids, empty failure kind, and negative phase", () =>
  Effect.gen(function* () {
    const emptyOperationId = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: { ...phaseFinished.payload, operationId: "" },
    }).pipe(Effect.flip);
    const emptyServiceId = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: {
        ...phaseFinished.payload,
        services: [{ serviceId: "", result: "completed" }],
      },
    }).pipe(Effect.flip);
    const emptyFailureKind = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: {
        ...phaseFinished.payload,
        services: [
          { serviceId: "worker", result: "failed", failure: { kind: "" } },
        ],
      },
    }).pipe(Effect.flip);
    const negativePhase = yield* decodeRuntimePhaseEvidence({
      ...phaseFinished,
      payload: { ...phaseFinished.payload, phase: -1 },
    }).pipe(Effect.flip);

    assert.strictEqual(emptyOperationId._tag, "RuntimePhaseEvidenceInvalid");
    assert.strictEqual(emptyServiceId._tag, "RuntimePhaseEvidenceInvalid");
    assert.strictEqual(emptyFailureKind._tag, "RuntimePhaseEvidenceInvalid");
    assert.strictEqual(negativePhase._tag, "RuntimePhaseEvidenceInvalid");
  }),
);
