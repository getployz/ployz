/**
 * What a Machine and Cloud agree on when the Machine signs a request with its management key. The Machine's code
 * holds the other copy of these values.
 */

/** How far a Machine's clock may sit from Cloud's, in seconds, for either kind of signed request. */
export const MAX_SKEW_SECONDS = 300;

/** OTLP routes: a bearer token, `ployz-otlp-v1.<payload>.<signature>`, signed over the domain and the payload segment. */
export const OTLP_BEARER = /^Bearer ployz-otlp-v1\.([A-Za-z0-9_-]{1,512})\.([A-Za-z0-9_-]{86})$/;
export const OTLP_DOMAIN = Buffer.from("ployz-otlp-v1\0", "ascii");
export const OTLP_TOKEN_MAX_LIFETIME_SECONDS = 86_400;

/** Push routes: `ployz-timestamp` and `ployz-signature` headers, signed over the domain, Machine id, timestamp and body hash. */
export const PUSH_TIMESTAMP = /^(0|[1-9]\d{0,11})$/;
export const PUSH_SIGNATURE = /^v1=([A-Za-z0-9_-]{86})$/;
export const PUSH_DOMAIN = Buffer.from("ployz-push-v1\0", "ascii");
