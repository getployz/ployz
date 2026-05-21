import installScript from "./ployz.txt";

const repo = "getployz/ployz";
const payloadAssetPattern =
  /^ployz-payload-(linux|darwin)-(x86_64|aarch64)\.tar\.gz$/;

function response(body: string) {
  return new Response(body, {
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "content-disposition": 'attachment; filename="ployz.sh"',
      "cache-control": "public, max-age=300",
      "x-content-type-options": "nosniff",
    },
  });
}

function notFound() {
  return new Response("Not found\n", {
    status: 404,
    headers: {
      "content-type": "text/plain; charset=utf-8",
      "x-content-type-options": "nosniff",
    },
  });
}

async function proxyReleasePayload(pathname: string) {
  const latestMatch = pathname.match(/^\/releases\/latest\/download\/([^/]+)$/);
  const versionMatch = pathname.match(/^\/releases\/download\/([^/]+)\/([^/]+)$/);
  const version = latestMatch ? "latest" : versionMatch?.[1];
  const asset = latestMatch?.[1] ?? versionMatch?.[2];

  if (!version || !asset || !payloadAssetPattern.test(asset)) {
    return notFound();
  }

  const upstreamUrl =
    version === "latest"
      ? `https://github.com/${repo}/releases/latest/download/${asset}`
      : `https://github.com/${repo}/releases/download/${version}/${asset}`;

  if (version === "latest") {
    const upstream = await fetch(upstreamUrl, { method: "HEAD", redirect: "manual" });
    const location = upstream.headers.get("location");
    const latestVersion = location?.match(/\/releases\/download\/([^/]+)\//)?.[1];

    if (!latestVersion) {
      return new Response("Could not resolve latest release\n", {
        status: 502,
        headers: {
          "content-type": "text/plain; charset=utf-8",
          "cache-control": "no-store",
          "x-content-type-options": "nosniff",
        },
      });
    }

    return new Response(null, {
      status: 302,
      headers: {
        location: `/releases/download/${latestVersion}/${asset}`,
        "cache-control": "no-store",
        "x-content-type-options": "nosniff",
      },
    });
  }

  const upstream = await fetch(upstreamUrl, {
    redirect: "follow",
    cf: { cacheTtl: 300, cacheEverything: true },
  });
  const headers = new Headers(upstream.headers);
  headers.delete("set-cookie");
  headers.set("x-content-type-options", "nosniff");

  return new Response(upstream.body, {
    status: upstream.status,
    statusText: upstream.statusText,
    headers,
  });
}

export default {
  fetch(request: Request) {
    const { pathname } = new URL(request.url);

    if (pathname === "/" || pathname === "/ployz.sh" || pathname === "/ployz.txt") {
      return response(installScript);
    }

    if (pathname.startsWith("/releases/")) {
      return proxyReleasePayload(pathname);
    }

    return notFound();
  },
};
