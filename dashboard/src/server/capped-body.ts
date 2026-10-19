/** The body's bytes, or null once it says or proves it is over `maxBytes`. Stops reading at the cap. */
export async function readCappedBody(request: Request, maxBytes: number) {
  if (Number(request.headers.get("content-length")) > maxBytes) return null;
  if (request.body === null) return new Uint8Array(0);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (let read = await reader.read(); !read.done; read = await reader.read()) {
    size += read.value.byteLength;
    if (size > maxBytes) {
      await reader.cancel();
      return null;
    }
    chunks.push(read.value);
  }
  return Buffer.concat(chunks);
}
