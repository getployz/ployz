import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { Effect, Result as EffectResult } from "effect";
import { Inngest } from "inngest";
import type { GithubBranchCursor } from "#/modules/github/github-ingestion.repository";
import { planGithubBranchEvaluation } from "#/modules/github/github-branch-evaluation";
import {
  runGithubRepositoryResult as runGithubRepositoryResultWithHarness,
  savedGithubServiceNode,
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import * as repository from "#/modules/github/github-ingestion.repository";
import * as deliveryRepository from "#/modules/github/github-ingestion.delivery.repository";
import { InngestClient } from "#/modules/inngest/client";

const environmentId = "00000000-0000-4000-8000-000000000001";
const serviceId = "00000000-0000-4000-8000-000000000011";
const serviceLineageId = "00000000-0000-4000-8000-000000000012";

describe("GitHub ingestion PostgreSQL persistence", () => {
  let harness: GithubPostgresTestHarness;
  const inngest = new Inngest({ id: "github-ingestion-repository-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const runGithubRepositoryResult = <Success, Failure>(
    operation: Effect.Effect<
      Success,
      Failure,
      import("#/server/database.server").Database | InngestClient
    >,
  ) =>
    runGithubRepositoryResultWithHarness(
      harness,
      operation.pipe(Effect.provideService(InngestClient, inngest)),
    );

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness.stop();
  });

  beforeEach(async () => {
    const node = savedGithubServiceNode({
      serviceId,
      lineageId: serviceLineageId,
    });
    const { env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...config } = node.config;
    void _env;
    void _mounts;
    const intent = {
      version: 1,
      environmentSlug: "production",
      services: [
        {
          id: serviceId,
          lineageId: serviceLineageId,
          slug: "api",
          config,
          variables: [],
          variableGroupAttachments: [],
          volumeAttachments: [],
          encryptedRegistryUsername: null,
          encryptedRegistrySecret: null,
        },
      ],
      variableGroups: [],
      volumes: [],
    };
    await harness.pool.query(`
      truncate table github_environment_trigger, github_branch_projection,
        github_check_suite_projection, github_webhook_delivery restart identity;
      delete from environment;
      delete from project;
      delete from organization;
      insert into organization (id, name, slug)
      values ('00000000-0000-4000-8000-000000000101', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name)
      values (
        '00000000-0000-4000-8000-000000000103',
        'github-acceptance@example.com', 'GitHub Acceptance'
      ) on conflict (id) do nothing;
      insert into project (id, organization_id, name, slug)
      values (
        '00000000-0000-4000-8000-000000000102',
        '00000000-0000-4000-8000-000000000101',
        'GitHub',
        'github'
      );
      insert into environment (id, project_id, organization_id, name, namespace, intent
      ) values (
        '${environmentId}',
        '00000000-0000-4000-8000-000000000102',
        '00000000-0000-4000-8000-000000000101',
        'Production',
        'production', '{"version":1,"environmentSlug":"production","services":[],"variableGroups":[],"volumes":[]}'
      );
      insert into environment_saved_state_snapshot (
        id, organization_id, environment_id, actor_id, message,
        intent, volume_deletion_authorizations
      ) values (
        '00000000-0000-4000-8000-000000000104',
        '00000000-0000-4000-8000-000000000101', '${environmentId}',
        '00000000-0000-4000-8000-000000000103', 'GitHub acceptance',
        '${JSON.stringify(intent)}'::jsonb, '[]'::jsonb
      );
    `);
  });

  it("emits a fresh environment trigger when a branch returns to an earlier SHA", async () => {
    let cursor: GithubBranchCursor | null = null;
    const heads = ["a".repeat(40), "b".repeat(40), "a".repeat(40)];

    for (const [index, headSha] of heads.entries()) {
      const deliveryId = `branch-cycle-${index + 1}`;
      const receipt = await runGithubRepositoryResult(
        repository.recordGithubDelivery({
        deliveryId,
        eventKind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        branch: { state: "active", headSha },
        }),
      );
      expect(EffectResult.isSuccess(receipt)).toBe(true);
      if (EffectResult.isFailure(receipt)) return;
      const processingRunId = `run-${index + 1}`;
      const claim = await runGithubRepositoryResult(
        repository.claimGithubDelivery({
        deliveryId,
          receiptSequence: receipt.success.receiptSequence,
        processingRunId,
        }),
      );
      expect(EffectResult.isSuccess(claim)).toBe(true);
      const plan = planGithubBranchEvaluation({
        cursor,
        liveBranch: {
          state: "present",
          ref: "refs/heads/main",
          headSha,
        },
        forced: index > 0,
        comparison: { state: "not_required" },
        candidates: [{ environmentId, serviceId, watchPaths: [] }],
      });
      expect(EffectResult.isSuccess(plan)).toBe(true);
      if (EffectResult.isFailure(plan)) return;
      const apply = repository.applyGithubBranchEvaluation({
        deliveryId,
        receiptSequence: receipt.success.receiptSequence,
        processingRunId,
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        expectedCursor: cursor,
        plan: plan.success,
      });
      const applied = await runGithubRepositoryResult(apply);
      expect(EffectResult.isSuccess(applied)).toBe(true);
      if (EffectResult.isFailure(applied)) return;
      cursor = applied.success.cursor;
      await harness.pool.query(
        "update environment_deployment set status = 'applied', finished_at = now() where environment_id = $1 and status = 'queued'",
        [environmentId],
      );
    }

    const rows = await harness.pool.query<{
      head_sha: string;
      trigger_revision: number;
    }>(`
      select head_sha, trigger_revision::integer as trigger_revision
      from github_environment_trigger
      order by trigger_revision
    `);
    expect(rows.rows).toEqual([
      { head_sha: "a".repeat(40), trigger_revision: 1 },
      { head_sha: "b".repeat(40), trigger_revision: 2 },
      { head_sha: "a".repeat(40), trigger_revision: 3 },
    ]);
    const deployments = await harness.pool.query<{
      status: string;
      trigger_origin: {
        origin: string;
        deliveryId: string;
        branchEvaluationRevision: number;
      };
    }>(`
      select status, trigger_origin
      from environment_deployment
      order by (trigger_origin->>'branchEvaluationRevision')::integer
    `);
    expect(deployments.rows).toEqual(
      rows.rows.map((row) => ({
        status: "applied",
        trigger_origin: {
          origin: "github",
          deliveryId: `branch-cycle-${row.trigger_revision}`,
          branchEvaluationRevision: row.trigger_revision,
          installationId: 17,
          repositoryId: 42,
        },
      })),
    );

    const terminalReplay = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId: "branch-cycle-3",
      eventKind: "push",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active", headSha: "a".repeat(40) },
      }),
    );
    expect(
      EffectResult.isSuccess(terminalReplay) && terminalReplay.success,
    ).toMatchObject({
      disposition: "terminal",
      evidence: { state: "processed", outcome: "branch_rebased_all_services" },
    });
  });

  it("recovers received work, preserves run ownership, and terminally cancels it", async () => {
    const input = {
      deliveryId: "received-recovery",
      eventKind: "push" as const,
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active" as const, headSha: "a".repeat(40) },
    };
    const recorded = await runGithubRepositoryResult(
      repository.recordGithubDelivery(input),
    );
    const receivedReplay = await runGithubRepositoryResult(
      repository.recordGithubDelivery(input),
    );
    expect(
      EffectResult.isSuccess(receivedReplay) && receivedReplay.success,
    ).toMatchObject({
      disposition: "replayed",
      state: "received",
      processingRunId: null,
    });
    if (EffectResult.isFailure(recorded)) return;

    const claimed = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      processingRunId: "run-recovery",
      }),
    );
    const resumed = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      processingRunId: "run-recovery",
      }),
    );
    const ownedElsewhere = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      processingRunId: "run-other",
      }),
    );
    expect(EffectResult.isSuccess(claimed) && claimed.success).toMatchObject({
      disposition: "claimed",
    });
    expect(EffectResult.isSuccess(resumed) && resumed.success).toMatchObject({
      disposition: "resumed",
    });
    expect(
      EffectResult.isSuccess(ownedElsewhere) && ownedElsewhere.success,
    ).toMatchObject({
      disposition: "owned_elsewhere",
      runId: "run-recovery",
    });

    const cancelled = await runGithubRepositoryResult(
      repository.cancelGithubDelivery({
      processingRunId: "run-recovery",
      }),
    );
    const terminalReplay = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      processingRunId: "run-recovery",
      }),
    );
    expect(EffectResult.isSuccess(cancelled) && cancelled.success).toEqual({
      disposition: "terminal",
    });
    expect(
      EffectResult.isSuccess(terminalReplay) && terminalReplay.success,
    ).toMatchObject({
      disposition: "terminal",
      evidence: {
        state: "cancelled",
        outcome: "cancelled",
        failureCode: "inngest_cancelled",
      },
    });

    const identityConflict = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      repositoryId: 43,
      processingRunId: "run-recovery",
      }),
    );
    expect(
      EffectResult.isFailure(identityConflict) && identityConflict.failure,
    ).toMatchObject({
      code: "delivery_conflict",
    });
  });

  it("persists typed failure evidence for the owning run", async () => {
    const input = {
      deliveryId: "failed-observation",
      eventKind: "check_suite" as const,
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 7001,
      checkSuiteAction: "completed" as const,
      headSha: "c".repeat(40),
      checkSuiteStatus: "completed" as const,
      checkSuiteConclusion: "failure" as const,
    };
    const recorded = await runGithubRepositoryResult(
      repository.recordGithubDelivery(input),
    );
    if (EffectResult.isFailure(recorded)) return;
    await runGithubRepositoryResult(
      repository.claimGithubDelivery({
      deliveryId: input.deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-failed",
      }),
    );

    const failed = await runGithubRepositoryResult(
      repository.failGithubDelivery({
      processingRunId: "run-failed",
      failureCode: "observation_failed",
      }),
    );
    const replay = await runGithubRepositoryResult(
      repository.recordGithubDelivery(input),
    );

    expect(EffectResult.isSuccess(failed) && failed.success).toEqual({
      disposition: "terminal",
    });
    expect(EffectResult.isSuccess(replay) && replay.success).toMatchObject({
      disposition: "terminal",
      evidence: {
        state: "failed",
        outcome: "processing_failed",
        failureCode: "observation_failed",
      },
    });
  });

  it.each([
    ["push", "failed"] as const,
    ["push", "cancelled"] as const,
    ["check_suite", "failed"] as const,
    ["check_suite", "cancelled"] as const,
  ])(
    "terminally marks and idempotently replays %s deliveries as %s by run ownership",
    async (eventKind, terminalState) => {
      const deliveryId = `${eventKind}-${terminalState}-lifecycle`;
      const input =
        eventKind === "push"
          ? {
              deliveryId,
              eventKind,
              installationId: 17,
              repositoryId: 42,
              ref: "refs/heads/main",
              branch: { state: "active" as const, headSha: "a".repeat(40) },
            }
          : {
              deliveryId,
              eventKind,
              installationId: 17,
              repositoryId: 42,
              checkSuiteId: 7001,
              checkSuiteAction: "completed" as const,
              headSha: "c".repeat(40),
              checkSuiteStatus: "completed" as const,
              checkSuiteConclusion: "success" as const,
            };
      const admitted = await runGithubRepositoryResult(
        repository.recordAndClaimGithubDelivery({
        ...input,
        processingRunId: "original-lifecycle-run",
        }),
      );
      expect(
        EffectResult.isSuccess(admitted) && admitted.success,
      ).toMatchObject({
        disposition: "claimed",
      });
      const active = await harness.pool.query<{ processing_state: string }>(
        `select processing_state from github_webhook_delivery
         where delivery_id = $1`,
        [deliveryId],
      );
      expect(active.rows).toEqual([{ processing_state: "processing" }]);
      const wrongRun = await runGithubRepositoryResult(
        terminalState === "failed"
          ? repository.failGithubDelivery({
              processingRunId: "handler-lifecycle-run",
              failureCode: "retry_exhausted",
            })
          : repository.cancelGithubDelivery({
              processingRunId: "handler-lifecycle-run",
            }),
      );
      expect(EffectResult.isSuccess(wrongRun) && wrongRun.success).toEqual({
        disposition: "not_found",
      });

      const terminalize = () =>
        runGithubRepositoryResult(
        terminalState === "failed"
          ? repository.failGithubDelivery({
              processingRunId: "original-lifecycle-run",
              failureCode: "retry_exhausted",
            })
          : repository.cancelGithubDelivery({
              processingRunId: "original-lifecycle-run",
              }),
        );
      const first = await terminalize();
      const repeated = await terminalize();
      expect(EffectResult.isSuccess(first) && first.success).toEqual({
        disposition: "terminal",
      });
      expect(EffectResult.isSuccess(repeated) && repeated.success).toEqual({
        disposition: "terminal",
      });

      const replay = await runGithubRepositoryResult(
        repository.recordGithubDelivery(input),
      );
      expect(EffectResult.isSuccess(replay) && replay.success).toMatchObject({
        disposition: "terminal",
        evidence:
          terminalState === "failed"
            ? {
                state: "failed",
                outcome: "processing_failed",
                failureCode: "retry_exhausted",
              }
            : {
                state: "cancelled",
                outcome: "cancelled",
                failureCode: "inngest_cancelled",
              },
      });
    },
  );

  it("rejects completion fallback for a matching run in the wrong terminal state", async () => {
    const input = {
      deliveryId: "failed-completion-fallback",
      eventKind: "push" as const,
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active" as const, headSha: "a".repeat(40) },
      processingRunId: "run-failed-fallback",
    };
    const admitted = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery(input),
    );
    if (EffectResult.isFailure(admitted)) throw admitted.failure;
    await runGithubRepositoryResult(
      repository.failGithubDelivery({
      processingRunId: input.processingRunId,
      failureCode: "retry_exhausted",
      }),
    );

    const completed = await runGithubRepositoryResult(
      deliveryRepository.completeGithubDelivery(
        {
          deliveryId: input.deliveryId,
          receiptSequence: admitted.success.receiptSequence,
          processingRunId: input.processingRunId,
          identity: {
            eventKind: "push",
            installationId: 17,
            repositoryId: 42,
            ref: "refs/heads/main",
          },
        },
        "ignored_stale",
      ),
    );
    expect(
      EffectResult.isFailure(completed) && completed.failure,
    ).toMatchObject({
      code: "run_conflict",
    });
  });

  it("retains a committed push outcome when replay recomputation becomes ignored stale", async () => {
    const input = {
      deliveryId: "push-commit-then-crash",
      eventKind: "push" as const,
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active" as const, headSha: "a".repeat(40) },
    };
    const recorded = await runGithubRepositoryResult(
      repository.recordAndClaimGithubDelivery({
      ...input,
      processingRunId: "run-push-crash",
      }),
    );
    if (EffectResult.isFailure(recorded)) throw recorded.failure;
    const plan = planGithubBranchEvaluation({
      cursor: null,
      liveBranch: {
        state: "present",
        ref: input.ref,
        headSha: input.branch.headSha,
      },
      forced: false,
      comparison: { state: "not_required" },
      candidates: [{ environmentId, serviceId, watchPaths: [] }],
    });
    if (EffectResult.isFailure(plan)) throw plan.failure;
    const committed = await runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
      deliveryId: input.deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-push-crash",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      expectedCursor: null,
      plan: plan.success,
      }),
    );
    if (EffectResult.isFailure(committed) || !committed.success.cursor) {
      throw EffectResult.isFailure(committed)
        ? committed.failure
        : new Error("Expected a committed cursor.");
    }
    const replayed = await runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
      deliveryId: input.deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-push-crash",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
        expectedCursor: committed.success.cursor,
      plan: { kind: "stale", outcome: "ignored_stale" },
      }),
    );
    expect(
      EffectResult.isSuccess(committed) && committed.success,
    ).toMatchObject({
      disposition: "applied",
    });
    expect(EffectResult.isSuccess(replayed) && replayed.success).toMatchObject({
      disposition: "stale",
    });

    const canonicalCompletion = {
      deliveryId: input.deliveryId,
      receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-push-crash",
      identity: {
        eventKind: "push" as const,
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
      },
    };
    const mismatches = [
      {
        ...canonicalCompletion,
        receiptSequence: recorded.success.receiptSequence + 1,
      },
      { ...canonicalCompletion, processingRunId: "run-push-other" },
      {
        ...canonicalCompletion,
        identity: { ...canonicalCompletion.identity, ref: "refs/heads/other" },
      },
    ];
    for (const mismatch of mismatches) {
      const result = await runGithubRepositoryResult(
        deliveryRepository.completeGithubDelivery(mismatch, "ignored_stale"),
      );
      expect(EffectResult.isFailure(result) && result.failure).toMatchObject({
        code: "run_conflict",
      });
    }
    const terminalReplay = await runGithubRepositoryResult(
      repository.recordGithubDelivery(input),
    );
    expect(
      EffectResult.isSuccess(terminalReplay) && terminalReplay.success,
    ).toMatchObject({
      evidence: { state: "processed", outcome: "branch_projected" },
    });
  });

  it("retains a committed suite outcome when replay recomputation becomes unchanged", async () => {
    const apply = async (deliveryId: string, runId: string) => {
      const input = {
        deliveryId,
        eventKind: "check_suite" as const,
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 7001,
        checkSuiteAction: "completed" as const,
        headSha: "c".repeat(40),
        checkSuiteStatus: "completed" as const,
        checkSuiteConclusion: "success" as const,
      };
      const recorded = await runGithubRepositoryResult(
        repository.recordAndClaimGithubDelivery({
        ...input,
        processingRunId: runId,
        }),
      );
      if (EffectResult.isFailure(recorded)) throw recorded.failure;
      const testimony = () =>
        runGithubRepositoryResult(
        repository.applyGithubCheckSuiteTestimony({
          deliveryId,
            receiptSequence: recorded.success.receiptSequence,
          processingRunId: runId,
          installationId: 17,
          repositoryId: 42,
          checkSuiteId: 7001,
          headSha: "c".repeat(40),
          status: "completed",
          conclusion: "success",
          sourceUpdatedAt: new Date("2026-07-16T08:00:00Z"),
          }),
        );
      return { result: await testimony(), testimony };
    };
    const initial = await apply("suite-crash-initial", "run-suite-initial");
    expect(EffectResult.isSuccess(initial.result)).toBe(true);
    const acknowledged = await runGithubRepositoryResult(
      repository.acknowledgeGithubCheckSuiteTransition({
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 7001,
        transitionRevision: 1,
      }),
    );
    expect(EffectResult.isSuccess(acknowledged)).toBe(true);

    const replayed = await initial.testimony();
    expect(EffectResult.isSuccess(replayed) && replayed.success).toMatchObject({
      disposition: "unchanged",
    });
    const terminalReplay = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId: "suite-crash-initial",
      eventKind: "check_suite",
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 7001,
      checkSuiteAction: "completed",
      headSha: "c".repeat(40),
      checkSuiteStatus: "completed",
      checkSuiteConclusion: "success",
      }),
    );
    expect(
      EffectResult.isSuccess(terminalReplay) && terminalReplay.success,
    ).toMatchObject({
      evidence: { state: "processed", outcome: "check_suite_projected" },
    });
  });

  it("loads one suite predecessor despite more than 200 unrelated pending rows", async () => {
    await harness.pool.query(`
      insert into github_webhook_delivery (
        delivery_id, event_kind, processing_state, outcome,
        installation_id, repository_id, head_sha, check_suite_id,
        check_suite_action, check_suite_status, processing_run_id,
        processing_started_at, processed_at
      )
      select
        'bulk-check-' || suite_id,
        'check_suite',
        'processed',
        'check_suite_projected',
        17,
        42,
        repeat('c', 40),
        suite_id,
        'completed',
        'completed',
        'run-bulk-' || suite_id,
        now(),
        now()
      from (
        select generate_series(1, 201)::bigint as suite_id
        union all select 999::bigint
      ) suites;

      insert into github_check_suite_projection (
        installation_id, repository_id, check_suite_id, head_sha, status,
        conclusion, source_updated_at, last_delivery_id,
        last_receipt_sequence, transition_revision, published_revision,
        updated_at
      )
      select
        17,
        42,
        delivery.check_suite_id,
        delivery.head_sha,
        'completed',
        'success',
        now(),
        delivery.delivery_id,
        delivery.receipt_sequence,
        1,
        0,
        now() + delivery.check_suite_id * interval '1 second'
      from github_webhook_delivery delivery;
    `);

    const globalPage = await runGithubRepositoryResult(
      repository.listPendingGithubCheckSuiteTransitions({
      limit: 100,
      }),
    );
    const exact = await runGithubRepositoryResult(
      repository.loadPendingGithubCheckSuiteTransition({
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 999,
      }),
    );

    expect(EffectResult.isSuccess(globalPage)).toBe(true);
    if (EffectResult.isSuccess(globalPage)) {
      expect(globalPage.success).toHaveLength(100);
      expect(globalPage.success.some((row) => row.checkSuiteId === 999)).toBe(
        false,
      );
    }
    expect(EffectResult.isSuccess(exact) && exact.success).toMatchObject({
      checkSuiteId: 999,
      transitionRevision: 1,
      sourceDeliveryId: "bulk-check-999",
    });
  });

  it("publishes the exact predecessor before the mandatory final suite apply", async () => {
    const apply = async (input: {
      deliveryId: string;
      processingRunId: string;
      status: "completed" | "queued";
      conclusion: "success" | null;
      sourceUpdatedAt: Date;
    }) => {
      const delivery = {
        deliveryId: input.deliveryId,
        eventKind: "check_suite" as const,
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 7001,
        checkSuiteAction: "completed" as const,
        headSha: "c".repeat(40),
        checkSuiteStatus: input.status,
        checkSuiteConclusion: input.conclusion,
      };
      const recorded = await runGithubRepositoryResult(
        repository.recordGithubDelivery(delivery),
      );
      if (EffectResult.isFailure(recorded)) throw recorded.failure;
      await runGithubRepositoryResult(
        repository.claimGithubDelivery({
        deliveryId: input.deliveryId,
          receiptSequence: recorded.success.receiptSequence,
        processingRunId: input.processingRunId,
        }),
      );
      return runGithubRepositoryResult(
        repository.applyGithubCheckSuiteTestimony({
        deliveryId: input.deliveryId,
          receiptSequence: recorded.success.receiptSequence,
        processingRunId: input.processingRunId,
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 7001,
        headSha: "c".repeat(40),
        status: input.status,
        conclusion: input.conclusion,
        sourceUpdatedAt: input.sourceUpdatedAt,
        }),
      );
    };
    const first = await apply({
      deliveryId: "suite-first",
      processingRunId: "run-suite-first",
      status: "completed",
      conclusion: "success",
      sourceUpdatedAt: new Date("2026-07-16T07:00:00Z"),
    });
    expect(EffectResult.isSuccess(first) && first.success).toMatchObject({
      transitionRevision: 1,
    });

    const secondInput = {
      deliveryId: "suite-second",
      processingRunId: "run-suite-second",
      status: "queued" as const,
      conclusion: null,
      sourceUpdatedAt: new Date("2026-07-16T07:00:01Z"),
    };
    const blocked = await apply(secondInput);
    expect(EffectResult.isFailure(blocked) && blocked.failure).toMatchObject({
      code: "pending_publication",
      retriable: true,
    });
    const predecessor = await runGithubRepositoryResult(
      repository.loadPendingGithubCheckSuiteTransition({
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 7001,
      }),
    );
    expect(
      EffectResult.isSuccess(predecessor) && predecessor.success,
    ).toMatchObject({
      transitionRevision: 1,
      sourceDeliveryId: "suite-first",
    });
    const acknowledged = await runGithubRepositoryResult(
      repository.acknowledgeGithubCheckSuiteTransition({
        installationId: 17,
        repositoryId: 42,
        checkSuiteId: 7001,
        transitionRevision: 1,
      }),
    );
    expect(
      EffectResult.isSuccess(acknowledged) && acknowledged.success,
    ).toEqual({
      acknowledged: true,
    });

    const replay = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId: secondInput.deliveryId,
      eventKind: "check_suite",
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 7001,
      checkSuiteAction: "completed",
      headSha: "c".repeat(40),
      checkSuiteStatus: secondInput.status,
      checkSuiteConclusion: secondInput.conclusion,
      }),
    );
    expect(EffectResult.isSuccess(replay) && replay.success).toMatchObject({
      disposition: "replayed",
      state: "processing",
      processingRunId: secondInput.processingRunId,
    });
    if (EffectResult.isFailure(replay)) return;
    const finalApply = await runGithubRepositoryResult(
      repository.applyGithubCheckSuiteTestimony({
      deliveryId: secondInput.deliveryId,
        receiptSequence: replay.success.receiptSequence,
      processingRunId: secondInput.processingRunId,
      installationId: 17,
      repositoryId: 42,
      checkSuiteId: 7001,
      headSha: "c".repeat(40),
      status: secondInput.status,
      conclusion: secondInput.conclusion,
      sourceUpdatedAt: secondInput.sourceUpdatedAt,
      }),
    );
    expect(EffectResult.isSuccess(finalApply) && finalApply.success).toEqual({
      disposition: "applied",
      transitionRevision: 2,
    });
  });

  it("rolls back a branch cursor, trigger, and terminal evidence together", async () => {
    const deliveryId = "rollback-branch";
    const recorded = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId,
      eventKind: "push",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active", headSha: "a".repeat(40) },
      }),
    );
    if (EffectResult.isFailure(recorded)) return;
    await runGithubRepositoryResult(
      repository.claimGithubDelivery({
      deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-rollback",
      }),
    );
    const plan = planGithubBranchEvaluation({
      cursor: null,
      liveBranch: {
        state: "present",
        ref: "refs/heads/main",
        headSha: "a".repeat(40),
      },
      forced: false,
      comparison: { state: "not_required" },
      candidates: [
        {
          environmentId,
          serviceId,
          watchPaths: [],
        },
      ],
    });
    if (EffectResult.isFailure(plan)) return;

    await harness.pool.query(`
      create function reject_github_deployment() returns trigger
      language plpgsql as $$ begin raise exception 'injected request failure'; end $$;
      create trigger reject_github_deployment
      before insert on environment_deployment
      for each row execute function reject_github_deployment();
    `);
    const applied = await runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
      deliveryId,
        receiptSequence: recorded.success.receiptSequence,
      processingRunId: "run-rollback",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      expectedCursor: null,
      plan: plan.success,
      }),
    );
    await harness.pool.query(`
      drop trigger reject_github_deployment on environment_deployment;
      drop function reject_github_deployment();
    `);

    expect(EffectResult.isFailure(applied)).toBe(true);
    const cursor = await runGithubRepositoryResult(
      repository.loadGithubBranchCursor({
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      }),
    );
    expect(EffectResult.isSuccess(cursor) && cursor.success).toBeNull();
    const row = await harness.pool.query<{
      processing_state: string;
      outcome: string | null;
    }>(
      "select processing_state, outcome from github_webhook_delivery where delivery_id = $1",
      [deliveryId],
    );
    expect(row.rows[0]).toEqual({
      processing_state: "processing",
      outcome: null,
    });
    const persisted = await harness.pool.query<{ count: number }>(`
      select count(*)::integer as count from github_environment_trigger
    `);
    expect(persisted.rows[0]?.count).toBe(0);
  });

  it("serializes competing branch CAS updates so only one revision wins", async () => {
    const prepare = async (
      deliveryId: string,
      headSha: string,
      runId: string,
    ) => {
      const recorded = await runGithubRepositoryResult(
        repository.recordGithubDelivery({
        deliveryId,
        eventKind: "push",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        branch: { state: "active", headSha },
        }),
      );
      if (EffectResult.isFailure(recorded)) throw recorded.failure;
      await runGithubRepositoryResult(
        repository.claimGithubDelivery({
        deliveryId,
          receiptSequence: recorded.success.receiptSequence,
        processingRunId: runId,
        }),
      );
      return recorded.success.receiptSequence;
    };
    const initialSequence = await prepare(
      "cas-initial",
      "a".repeat(40),
      "run-cas-initial",
    );
    const initialPlan = planGithubBranchEvaluation({
      cursor: null,
      liveBranch: {
        state: "present",
        ref: "refs/heads/main",
        headSha: "a".repeat(40),
      },
      forced: false,
      comparison: { state: "not_required" },
      candidates: [],
    });
    if (EffectResult.isFailure(initialPlan)) return;
    await runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
      deliveryId: "cas-initial",
      receiptSequence: initialSequence,
      processingRunId: "run-cas-initial",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      expectedCursor: null,
      plan: initialPlan.success,
      }),
    );
    const ignoredReplay = await runGithubRepositoryResult(
      repository.recordGithubDelivery({
      deliveryId: "cas-initial",
      eventKind: "push",
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      branch: { state: "active", headSha: "a".repeat(40) },
      }),
    );
    expect(
      EffectResult.isSuccess(ignoredReplay) && ignoredReplay.success,
    ).toMatchObject({
      disposition: "terminal",
      evidence: {
        state: "processed",
        outcome: "branch_projected",
      },
    });
    const cursorResult = await runGithubRepositoryResult(
      repository.loadGithubBranchCursor({
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      }),
    );
    if (EffectResult.isFailure(cursorResult) || !cursorResult.success) return;
    const cursor = cursorResult.success;

    const [leftSequence, rightSequence] = await Promise.all([
      prepare("cas-left", "b".repeat(40), "run-cas-left"),
      prepare("cas-right", "c".repeat(40), "run-cas-right"),
    ]);
    const makePlan = (headSha: string) =>
      planGithubBranchEvaluation({
        cursor,
        liveBranch: {
          state: "present" as const,
          ref: "refs/heads/main",
          headSha,
        },
        forced: true,
        comparison: { state: "not_required" as const },
        candidates: [],
      });
    const leftPlan = makePlan("b".repeat(40));
    const rightPlan = makePlan("c".repeat(40));
    if (EffectResult.isFailure(leftPlan) || EffectResult.isFailure(rightPlan)) {
      return;
    }

    const results = await Promise.all([
      runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
        deliveryId: "cas-left",
        receiptSequence: leftSequence,
        processingRunId: "run-cas-left",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        expectedCursor: cursor,
        plan: leftPlan.success,
      }),
      ),
      runGithubRepositoryResult(
      repository.applyGithubBranchEvaluation({
        deliveryId: "cas-right",
        receiptSequence: rightSequence,
        processingRunId: "run-cas-right",
        installationId: 17,
        repositoryId: 42,
        ref: "refs/heads/main",
        expectedCursor: cursor,
        plan: rightPlan.success,
      }),
      ),
    ]);

    expect(results.filter(EffectResult.isSuccess)).toHaveLength(1);
    const errors = results.filter(EffectResult.isFailure);
    expect(errors).toHaveLength(1);
    expect(errors[0]?.failure).toMatchObject({ code: "cursor_conflict" });
    const winningCursor = await runGithubRepositoryResult(
      repository.loadGithubBranchCursor({
      installationId: 17,
      repositoryId: 42,
      ref: "refs/heads/main",
      }),
    );
    expect(
      EffectResult.isSuccess(winningCursor) && winningCursor.success,
    ).toMatchObject({
      evaluationRevision: 2,
    });
  });

  it("rejects ambiguous terminal evidence at the database boundary", async () => {
    await expect(
      harness.pool.query(`
        insert into github_webhook_delivery (
          delivery_id, event_kind, processing_state, outcome,
          installation_id, repository_id, ref, branch_state, head_sha,
          processed_at
        ) values (
          'invalid-terminal', 'push', 'failed', 'processing_failed',
          17, 42, 'refs/heads/main', 'active', repeat('a', 40), now()
        )
      `),
    ).rejects.toMatchObject({ code: "23514" });
  });

  it("allows unowned receipts but rejects duplicate non-null processing runs", async () => {
    await expect(
      harness.pool.query(`
        insert into github_webhook_delivery (delivery_id, event_kind)
        values ('nullable-run-a', 'push'), ('nullable-run-b', 'push')
      `),
    ).resolves.toMatchObject({ rowCount: 2 });

    await harness.pool.query(`
      insert into github_webhook_delivery (
        delivery_id, event_kind, processing_state, installation_id,
        repository_id, ref, branch_state, head_sha, processing_run_id,
        processing_started_at
      ) values (
        'owned-run-a', 'push', 'processing', 17, 42, 'refs/heads/main',
        'active', repeat('a', 40), 'one-processing-run', now()
      )
    `);
    await expect(
      harness.pool.query(`
        insert into github_webhook_delivery (
          delivery_id, event_kind, processing_state, installation_id,
          repository_id, ref, branch_state, head_sha, processing_run_id,
          processing_started_at
        ) values (
          'owned-run-b', 'push', 'processing', 17, 42, 'refs/heads/main',
          'active', repeat('b', 40), 'one-processing-run', now()
        )
      `),
    ).rejects.toMatchObject({ code: "23505" });
  });
});
