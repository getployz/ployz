export const phaseAwareDeployRequestFixture = {
  version: 1,
  target: {
    namespace_id: "production",
    services: [
      { service_id: "database", image: "postgres:18" },
      { service_id: "worker", image: "worker:sha-2" },
    ],
    volumes: {},
  },
  phases: [
    {
      services: [
        { service_id: "database", requirement: "required" },
        { service_id: "retired-worker", requirement: "opportunistic" },
      ],
    },
    {
      services: [
        { service_id: "worker", requirement: "opportunistic" },
      ],
    },
  ],
} as const;

export const completedPhaseAwareDeployResultFixture = {
  version: 1,
  outcome: "completed",
  phases: [
    {
      phase: 0,
      outcome: "completed",
      services: [
        { service_id: "old-worker", result: "removed" },
        { service_id: "cache", result: "unchanged" },
      ],
    },
  ],
} as const;

export const completedPhaseAwareDeployRequestFixture = {
  version: 1,
  target: {
    namespace_id: "production",
    services: [{ service_id: "cache" }],
  },
  phases: [
    {
      services: [
        { service_id: "old-worker", requirement: "opportunistic" },
        { service_id: "cache", requirement: "required" },
      ],
    },
  ],
} as const;

export const partialPhaseAwareDeployResultFixture = {
  version: 1,
  outcome: "failed",
  phases: [
    {
      phase: 0,
      outcome: "failed",
      services: [
        { service_id: "database", result: "applied" },
        {
          service_id: "worker",
          result: "failed",
          failure: {
            code: "healthcheck_failed",
            message: "Worker did not become healthy.",
          },
        },
      ],
    },
    {
      phase: 1,
      outcome: "skipped",
      services: [
        {
          service_id: "web",
          result: "skipped",
          reason: {
            code: "required_phase_failed",
            message: "A required Service failed in phase 0.",
          },
        },
      ],
    },
  ],
} as const;

export const requiredFailurePhaseAwareDeployRequestFixture = {
  version: 1,
  target: {
    namespace_id: "production",
    services: [
      { service_id: "database" },
      { service_id: "worker" },
      { service_id: "web" },
    ],
  },
  phases: [
    {
      services: [
        { service_id: "database", requirement: "required" },
        { service_id: "worker", requirement: "required" },
      ],
    },
    { services: [{ service_id: "web", requirement: "required" }] },
  ],
} as const;

export const opportunisticFailurePhaseAwareDeployRequestFixture = {
  version: 1,
  target: {
    namespace_id: "production",
    services: [
      { service_id: "worker" },
      { service_id: "web" },
    ],
  },
  phases: [
    { services: [{ service_id: "worker", requirement: "opportunistic" }] },
    { services: [{ service_id: "web", requirement: "required" }] },
  ],
} as const;

export const opportunisticFailurePhaseAwareDeployResultFixture = {
  version: 1,
  outcome: "partial",
  phases: [
    {
      phase: 0,
      outcome: "partial",
      services: [
        {
          service_id: "worker",
          result: "failed",
          failure: {
            code: "healthcheck_failed",
            message: "Worker did not become healthy.",
          },
        },
      ],
    },
    {
      phase: 1,
      outcome: "completed",
      services: [{ service_id: "web", result: "applied" }],
    },
  ],
} as const;

export const interruptedPhaseAwareDeployResultFixture = {
  version: 1,
  outcome: "interrupted",
  phases: [
    {
      phase: 0,
      outcome: "interrupted",
      services: [
        {
          service_id: "api",
          result: "interrupted",
          interruption: {
            code: "commit_ambiguous",
            message: "Runtime could not prove whether the commit landed.",
          },
        },
      ],
    },
    {
      phase: 1,
      outcome: "skipped",
      services: [
        {
          service_id: "web",
          result: "skipped",
          reason: {
            code: "deploy_interrupted",
            message: "An earlier phase was interrupted.",
          },
        },
      ],
    },
  ],
} as const;

export const interruptedPhaseAwareDeployRequestFixture = {
  version: 1,
  target: {
    namespace_id: "production",
    services: [{ service_id: "api" }, { service_id: "web" }],
  },
  phases: [
    { services: [{ service_id: "api", requirement: "required" }] },
    { services: [{ service_id: "web", requirement: "required" }] },
  ],
} as const;
