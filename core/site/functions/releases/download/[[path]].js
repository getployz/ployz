// Serves Ployz's GitHub release assets from ployz.sh, which has an IPv6 address; github.com and its
// asset hosts don't. The assets are passed through unchanged, so installers verify the signed
// checksums exactly as they do from GitHub.
const RELEASES = "https://github.com/getployz/ployz/releases/download/";
const ASSET = /^v[0-9A-Za-z.+-]+\/[0-9A-Za-z][0-9A-Za-z._+-]*$/;
const PASSED = ["content-type", "content-length", "etag", "last-modified"];

export async function onRequest({ request, params }) {
  if (request.method !== "GET" && request.method !== "HEAD") {
    return new Response("Method not allowed\n", { status: 405, headers: { allow: "GET, HEAD" } });
  }
  const path = [].concat(params.path ?? []).join("/");
  if (!ASSET.test(path)) {
    return new Response("Not found\n", { status: 404 });
  }
  let upstream;
  try {
    upstream = await fetch(RELEASES + path, {
      method: request.method,
      headers: { "user-agent": "ployz.sh-releases" },
    });
  } catch {
    return new Response("Could not reach GitHub releases\n", { status: 502 });
  }
  const headers = new Headers({ "cache-control": "no-store" });
  for (const name of PASSED) {
    const value = upstream.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  return new Response(upstream.body, { status: upstream.status, headers });
}
