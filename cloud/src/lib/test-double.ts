export function asTestDouble<TTarget>() {
  return <TValue>(value: TValue): TValue & TTarget =>
    value as TValue & TTarget;
}
