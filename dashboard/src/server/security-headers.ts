// Every response: no page of ours may be framed (clickjacking the Deploy and delete dialogs), always over HTTPS.
// img-src stops markup that slips past rendering from loading a picture off a host that logs the URL; only our own
// files and GitHub avatars load. connect-src stays unset: the PostHog proxy host is runtime config, not build input.
export const SECURITY_HEADERS = {
  "content-security-policy": "frame-ancestors 'none'; img-src 'self' data: blob: https://avatars.githubusercontent.com",
  "x-frame-options": "DENY",
  "x-content-type-options": "nosniff",
  "referrer-policy": "strict-origin-when-cross-origin",
  "strict-transport-security": "max-age=31536000",
} as const;
