import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetFreshDatabase } from "./db-migrate-fresh.mjs";

const mocks = {
  polarConstructor: vi.fn(),
  listCustomers: vi.fn(),
  deleteCustomer: vi.fn(),
  clientConstructor: vi.fn(),
  connect: vi.fn(),
  query: vi.fn(),
  end: vi.fn(),
};

describe("fresh database migration", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.listCustomers.mockResolvedValue([
      { result: { items: [{ id: "customer-1" }] } },
    ]);
    mocks.polarConstructor.mockImplementation(function Polar() {
      return {
        customers: {
          list: mocks.listCustomers,
          delete: mocks.deleteCustomer,
        },
      };
    });
    mocks.connect.mockResolvedValue(undefined);
    mocks.query.mockResolvedValue(undefined);
    mocks.end.mockResolvedValue(undefined);
    mocks.clientConstructor.mockImplementation(function Client() {
      return {
        connect: mocks.connect,
        query: mocks.query,
        end: mocks.end,
      };
    });
  });

  it.each([undefined, ""])(
    "resets Postgres without constructing Polar when the token is %s",
    async (polarAccessToken) => {
      await resetFreshDatabase({
        databaseUrl: "postgres://fresh-test",
        polarAccessToken,
        polarServer: "production",
        PolarClient: mocks.polarConstructor,
        PostgresClient: mocks.clientConstructor,
      });

      expect(mocks.polarConstructor).not.toHaveBeenCalled();
      expect(mocks.clientConstructor).toHaveBeenCalledWith({
        connectionString: "postgres://fresh-test",
      });
      expect(mocks.connect).toHaveBeenCalledOnce();
      expect(mocks.query.mock.calls.map(([query]) => query)).toEqual([
        "DROP SCHEMA IF EXISTS drizzle CASCADE",
        "DROP SCHEMA IF EXISTS public CASCADE",
        "CREATE SCHEMA public",
        "GRANT ALL ON SCHEMA public TO postgres",
        "GRANT ALL ON SCHEMA public TO public",
      ]);
      expect(mocks.end).toHaveBeenCalledOnce();
    },
  );

  it("deletes sandbox customers before resetting Postgres", async () => {
    await resetFreshDatabase({
      databaseUrl: "postgres://fresh-test",
      polarAccessToken: "sandbox-token",
      polarServer: "sandbox",
      PolarClient: mocks.polarConstructor,
      PostgresClient: mocks.clientConstructor,
    });

    expect(mocks.polarConstructor).toHaveBeenCalledWith({
      accessToken: "sandbox-token",
      server: "sandbox",
    });
    expect(mocks.listCustomers).toHaveBeenCalledWith({ limit: 100 });
    expect(mocks.deleteCustomer).toHaveBeenCalledWith({ id: "customer-1" });
    expect(mocks.deleteCustomer.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.connect.mock.invocationCallOrder[0],
    );
    expect(mocks.query).toHaveBeenCalledTimes(5);
    expect(mocks.end).toHaveBeenCalledOnce();
  });

  it.each(["production", undefined])(
    "rejects token use with POLAR_SERVER=%s before mutation",
    async (polarServer) => {
      await expect(
        resetFreshDatabase({
          databaseUrl: "postgres://fresh-test",
          polarAccessToken: "production-token",
          polarServer: polarServer ?? "production",
          PolarClient: mocks.polarConstructor,
          PostgresClient: mocks.clientConstructor,
        }),
      ).rejects.toThrow(
        "db-migrate-fresh can only delete Polar customers in sandbox",
      );

      expect(mocks.polarConstructor).not.toHaveBeenCalled();
      expect(mocks.clientConstructor).not.toHaveBeenCalled();
      expect(mocks.deleteCustomer).not.toHaveBeenCalled();
      expect(mocks.connect).not.toHaveBeenCalled();
      expect(mocks.query).not.toHaveBeenCalled();
    },
  );
});
