/** The credential of an `Authorization: Bearer` header, or `undefined` when the request carries none. */
export const bearerToken = (headers: Headers) => /^Bearer (\S+)$/u.exec(headers.get("authorization") ?? "")?.[1];
