export function managementAddress(publicKey: readonly number[]): string {
  const bytes = [0xfd, 0xcc, ...publicKey.slice(0, 14)];
  if (bytes.length !== 16) throw new Error("A WireGuard public key has 32 bytes.");
  const groups: string[] = [];
  for (let index = 0; index < 16; index += 2) {
    groups.push((((bytes[index] ?? 0) << 8) | (bytes[index + 1] ?? 0)).toString(16));
  }
  return groups.join(":");
}

export const dockerVolumeName = (namespace: string, volumeId: string) => `${namespace}_vol-${volumeId}`;

export function parseDockerVolumeName(name: string): { namespace: string; volumeId: string } | null {
  const at = name.lastIndexOf("_vol-");
  if (at <= 0) return null;
  const volumeId = name.slice(at + "_vol-".length);
  return volumeId.length === 0 ? null : { namespace: name.slice(0, at), volumeId };
}
