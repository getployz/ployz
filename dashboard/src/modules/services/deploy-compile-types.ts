export type ServiceMode =
  | {
      kind: "replicated";
      replicas: number;
    }
  | {
      kind: "global";
    };
