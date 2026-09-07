import { Data } from "effect";

export class DeployImageNotPullableError extends Data.TaggedError(
  "DeployImageNotPullableError",
)<{
  message: string;
  serviceIds: string[];
}> {
  readonly publicErrorCategory = "validation" as const;

  constructor(args: {
    services: ReadonlyArray<{ id: string; name: string }>;
  }) {
    const names = args.services.map((service) => service.name);
    const listed = names.join(", ");
    super({
      serviceIds: args.services.map((service) => service.id),
      message:
        names.length === 1
          ? `Deploy needs a pullable image (name, tag, or digest) for ${listed}. Git sources cannot be sent to the runtime yet.`
          : `Deploy needs pullable images (name, tag, or digest) for ${listed}. Git sources cannot be sent to the runtime yet.`,
    });
  }
}
