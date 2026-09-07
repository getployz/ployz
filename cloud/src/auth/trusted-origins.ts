function toOrigin(value: string) {
  const normalized = value.trim();

  if (!normalized) {
    return null;
  }

  if (normalized.includes("*") || normalized.includes("?")) {
    const withoutScheme = normalized.replace(/^https?:\/\//, "");
    const host = withoutScheme.split("/")[0];

    if (!host) {
      return null;
    }

    if (/^https?:\/\//.test(normalized)) {
      return `${normalized.startsWith("http://") ? "http" : "https"}://${host}`;
    }

    return `https://${host}`;
  }

  if (URL.canParse(normalized)) {
    return new URL(normalized).origin;
  }

  if (URL.canParse(`https://${normalized}`)) {
    return new URL(`https://${normalized}`).origin;
  }

  return null;
}

function toAllowedHost(value: string) {
  const normalized = value.trim();

  if (!normalized) {
    return null;
  }

  if (normalized.includes("*") || normalized.includes("?")) {
    return normalized.replace(/^https?:\/\//, "").split("/")[0] || null;
  }

  if (URL.canParse(normalized)) {
    return new URL(normalized).host;
  }

  return normalized.replace(/^https?:\/\//, "").split("/")[0] || null;
}

function splitOriginList(value?: string) {
  if (!value) {
    return [];
  }

  return value
    .split(",")
    .flatMap((entry) => {
      const origin = entry.trim();
      return origin ? [origin] : [];
    });
}

function isLocalhostHost(hostname: string) {
  return (
    hostname === "localhost" || hostname === "127.0.0.1" || hostname === "::1"
  );
}

export function getBetterAuthUrlConfig(
  appURL: string,
  configuredTrustedOrigins?: string,
  options: { trustLocalhost?: boolean } = {},
) {
  const appUrl = new URL(appURL);
  const appOrigin = appUrl.origin;
  const appHost = appUrl.host;
  const configuredOrigins = splitOriginList(configuredTrustedOrigins);
  const trustLocalhost =
    isLocalhostHost(appUrl.hostname) || options.trustLocalhost === true;
  const localDevOrigins = trustLocalhost
    ? ["http://localhost:*", "http://127.0.0.1:*"]
    : [];
  const localDevHosts = trustLocalhost
    ? ["localhost:*", "127.0.0.1:*"]
    : [];

  const trustedOrigins = Array.from(
    new Set([
      appOrigin,
      ...localDevOrigins,
      ...configuredOrigins
        .map((origin) => toOrigin(origin))
        .filter((origin): origin is string => origin !== null),
    ]),
  );

  const allowedHosts = Array.from(
    new Set([
      appHost,
      ...localDevHosts,
      ...configuredOrigins
        .map((origin) => toAllowedHost(origin))
        .filter((host): host is string => host !== null),
    ]),
  );

  return {
    allowedHosts,
    fallbackURL: appOrigin,
    trustedOrigins,
  };
}
