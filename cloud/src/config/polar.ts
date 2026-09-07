export type PolarConfiguration =
  | { mode: "self_hosted" }
  | {
      mode: "hosted";
      accessToken: string;
      webhookSecret: string;
      productIds: {
        free: string;
        solo: string;
        teams: string;
      };
    };

export type PolarEnvironment = {
  accessToken?: string;
  webhookSecret?: string;
  freeProductId?: string;
  soloProductId?: string;
  teamsProductId?: string;
  hobbyProductId?: string;
  proProductId?: string;
};

function resolveProductAlias(
  canonicalName: string,
  canonicalId: string | undefined,
  legacyName: string,
  legacyId: string | undefined,
) {
  if (canonicalId && legacyId && canonicalId !== legacyId) {
    throw new Error(
      `${canonicalName} conflicts with transitional alias ${legacyName}`,
    );
  }

  return canonicalId ?? legacyId;
}

export function resolvePolarConfiguration(
  input: PolarEnvironment,
): PolarConfiguration {
  const soloProductId = resolveProductAlias(
    "POLAR_PRODUCT_SOLO_ID",
    input.soloProductId,
    "POLAR_PRODUCT_HOBBY_ID",
    input.hobbyProductId,
  );
  const teamsProductId = resolveProductAlias(
    "POLAR_PRODUCT_TEAMS_ID",
    input.teamsProductId,
    "POLAR_PRODUCT_PRO_ID",
    input.proProductId,
  );
  const hostedValues = [
    input.accessToken,
    input.webhookSecret,
    input.freeProductId,
    soloProductId,
    teamsProductId,
  ];

  if (hostedValues.every((value) => value === undefined)) {
    return { mode: "self_hosted" };
  }
  if (
    input.accessToken === undefined ||
    input.webhookSecret === undefined ||
    input.freeProductId === undefined ||
    soloProductId === undefined ||
    teamsProductId === undefined
  ) {
    throw new Error(
      "Polar configuration must be entirely absent or include access token, webhook secret, and Free/Solo/Teams product IDs",
    );
  }
  if (new Set([input.freeProductId, soloProductId, teamsProductId]).size !== 3) {
    throw new Error("Free, Solo, and Teams must use distinct Polar product IDs");
  }

  return {
    mode: "hosted",
    accessToken: input.accessToken,
    webhookSecret: input.webhookSecret,
    productIds: {
      free: input.freeProductId,
      solo: soloProductId,
      teams: teamsProductId,
    },
  };
}
