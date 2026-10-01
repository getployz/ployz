import { describe, expect, it } from "vitest";
import { mountRefusal, replicaCap, replicaCount, volumeWriters, writersText } from "./volume-sharing";

const mount = (service: string) => ({ service, path: "/data" });
const replicas = (counts: Record<string, number>) => (service: string) => counts[service] ?? 1;

describe("replicaCap", () => {
  const data = { name: "data", mounts: [mount("db")], shared_writes: false };
  it("holds a Service to one replica while it mounts a Volume without shared writes", () => {
    expect(replicaCap("db", [data])?.name).toBe("data");
  });
  it("lets it scale once the Volume allows shared writes, or when it mounts none", () => {
    expect(replicaCap("db", [{ ...data, shared_writes: true }])).toBeNull();
    expect(replicaCap("web", [data])).toBeNull();
  });
});

describe("mountRefusal", () => {
  const data = { mounts: [mount("postgres")], shared_writes: false };
  it("refuses a second Service, and a Service with more than one replica", () => {
    expect(mountRefusal(data, "redis", 1)).toBe("Already used by postgres");
    expect(mountRefusal({ ...data, mounts: [] }, "web", 2)).toBe("Runs 2 replicas");
  });
  it("allows a first single writer, and anything once shared writes are on", () => {
    expect(mountRefusal({ ...data, mounts: [] }, "web", 1)).toBeNull();
    expect(mountRefusal({ ...data, shared_writes: true }, "redis", 3)).toBeNull();
  });
});

describe("volumeWriters", () => {
  it("is fine for one Service with one replica", () => {
    expect(volumeWriters({ mounts: [mount("db")] }, replicas({ db: 1 }))).toMatchObject({ total: 1, shared: false });
  });

  it("counts every replica of one Service", () => {
    const { total, shared, writers } = volumeWriters({ mounts: [mount("db")] }, replicas({ db: 2 }));
    expect({ total, shared }).toEqual({ total: 2, shared: true });
    expect(writersText(writers)).toBe("db ×2");
  });

  it("sums the Services that mount it", () => {
    const { total, writers } = volumeWriters({ mounts: [mount("postgres"), mount("redis")] }, replicas({ postgres: 2 }));
    expect(total).toBe(3);
    expect(writersText(writers)).toBe("postgres ×2, redis");
  });
});

describe("replicaCount", () => {
  it("reads a staged value over the default", () => {
    expect(replicaCount({ value: 2, default: 1 })).toBe(2);
    expect(replicaCount({ value: null, default: 1 })).toBe(1);
    expect(replicaCount(undefined)).toBe(1);
  });
});
