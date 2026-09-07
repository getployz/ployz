export type DialTenant = {
  relayUrl: string;
  bearer: string;
  pairing: string;
  preferredMachineId: string | null;
  enrolledMachineIds: readonly string[];
};

export function orderDialEntries(input: {
  preferredMachineId: string | null;
  enrolledMachineIds: readonly string[];
}): string[] {
  const enrolled = [...new Set(input.enrolledMachineIds)];
  if (
    input.preferredMachineId === null ||
    !enrolled.includes(input.preferredMachineId)
  ) {
    return enrolled;
  }
  return [
    input.preferredMachineId,
    ...enrolled.filter((id) => id !== input.preferredMachineId),
  ];
}
