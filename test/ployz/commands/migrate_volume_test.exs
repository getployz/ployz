defmodule Ployz.Commands.MigrateVolumeTest do
  use ExUnit.Case, async: false

  alias Ployz.Commands.MigrateVolume

  defmodule Commands do
    def create(receipt) do
      send(Process.get(:test_pid), {:command_create, receipt})
      :ok
    end

    def finish(receipt) do
      send(Process.get(:test_pid), {:command_finish, receipt})
      :ok
    end
  end

  defmodule Leases do
    def acquire(key, _owner, _ttl), do: {:ok, %{key: key, token: "lease-token"}}
    def assert_current(_key, "lease-token"), do: :ok

    def release(key, token) do
      send(Process.get(:test_pid), {:lease_release, key, token})
      :ok
    end
  end

  defmodule ExpiredLeases do
    def acquire(key, _owner, ttl) do
      send(Process.get(:test_pid), {:lease_ttl, ttl})
      {:ok, %{key: key, token: "expired-token"}}
    end

    def assert_current(_key, "expired-token"), do: {:error, {:lease_expired, 1234}}

    def release(key, token) do
      send(Process.get(:test_pid), {:lease_release, key, token})
      :ok
    end
  end

  defmodule Transaction do
    def transaction(fun), do: {:ok, fun.()}
  end

  defmodule Volumes do
    def next_generation("pgdata"), do: {:ok, 3}

    def promote_generation(row) do
      send(Process.get(:test_pid), {:promote, row})
      :ok
    end
  end

  defmodule Substrate do
    def snapshot("pgdata", _timeout), do: {:ok, %{snapshot_ref: "zfs://snap"}}
    def send(snapshot, _timeout), do: {:ok, Map.put(snapshot, :stream_ref, "zfs://stream")}

    def recv(stream, "machine-b", _timeout),
      do: {:ok, Map.put(stream, :dataset_ref, "zfs://machine-b/pgdata@3")}

    def verify(_transfer, _timeout), do: :ok
  end

  defmodule VerifyFailingSubstrate do
    def snapshot("pgdata", _timeout), do: {:ok, %{snapshot_ref: "zfs://snap"}}
    def send(snapshot, _timeout), do: {:ok, Map.put(snapshot, :stream_ref, "zfs://stream")}

    def recv(stream, "machine-b", _timeout),
      do: {:ok, Map.put(stream, :dataset_ref, "zfs://machine-b/pgdata@3")}

    def verify(_transfer, _timeout), do: {:error, :checksum_mismatch}
  end

  defmodule SnapshotFailingSubstrate do
    def snapshot("pgdata", _timeout), do: {:error, :snapshot_failed}
  end

  defmodule SendFailingSubstrate do
    def snapshot("pgdata", _timeout), do: {:ok, %{snapshot_ref: "zfs://snap"}}
    def send(_snapshot, _timeout), do: {:error, :send_failed}
  end

  defmodule ExplodingSubstrate do
    def snapshot("pgdata", _timeout), do: raise("zfs boom")
  end

  defmodule HeldLeases do
    def acquire(_key, _owner, _ttl), do: {:error, {:lease_held, :other, 1234}}
    def release(_key, _token), do: raise("lease should not be released when acquisition fails")
  end

  setup do
    Process.put(:test_pid, self())
    :ok
  end

  test "promotes a new volume generation after destination verification" do
    assert {:ok, receipt} =
             MigrateVolume.run(
               "pgdata",
               "machine-b",
               [command_id: "cmd-migrate"],
               deps(Substrate)
             )

    assert receipt.status == :committed
    assert receipt.generation == 3
    assert_received {:promote, %{volume: "pgdata", generation: 3, machine: "machine-b"}}
    assert_received {:command_finish, %{id: "cmd-migrate", status: :committed}}
  end

  test "does not promote when verification fails" do
    assert {:error, :checksum_mismatch, receipt} =
             MigrateVolume.run(
               "pgdata",
               "machine-b",
               [command_id: "cmd-migrate-fail"],
               deps(VerifyFailingSubstrate)
             )

    assert receipt.status == :failed
    refute_received {:promote, _row}
    assert_received {:command_finish, %{id: "cmd-migrate-fail", status: :failed}}
    assert_received {:lease_release, {:volume, "pgdata"}, "lease-token"}
  end

  test "does not promote when snapshot or send fails" do
    assert {:error, :snapshot_failed, receipt} =
             MigrateVolume.run(
               "pgdata",
               "machine-b",
               [command_id: "cmd-snapshot-fail"],
               deps(SnapshotFailingSubstrate)
             )

    assert receipt.status == :failed
    refute_received {:promote, _row}
    assert_received {:lease_release, {:volume, "pgdata"}, "lease-token"}

    assert {:error, :send_failed, receipt} =
             MigrateVolume.run(
               "pgdata",
               "machine-b",
               [command_id: "cmd-send-fail"],
               deps(SendFailingSubstrate)
             )

    assert receipt.status == :failed
    refute_received {:promote, _row}
  end

  test "lease contention prevents substrate work" do
    assert {:error, {:lease_held, :other, 1234}, receipt} =
             MigrateVolume.run("pgdata", "machine-b", [command_id: "cmd-held"], %{
               deps(Substrate)
               | leases: HeldLeases
             })

    assert receipt.status == :failed
    refute_received {:promote, _row}
  end

  test "substrate crashes release the lease and finish a failed receipt" do
    assert {:error, %{code: :command_crashed}, receipt} =
             MigrateVolume.run(
               "pgdata",
               "machine-b",
               [command_id: "cmd-crash"],
               deps(ExplodingSubstrate)
             )

    assert receipt.status == :failed
    assert_received {:lease_release, {:volume, "pgdata"}, "lease-token"}
  end

  test "lease must still be current before volume promotion" do
    assert {:error, %{code: :command_threw}, receipt} =
             MigrateVolume.run("pgdata", "machine-b", [command_id: "cmd-expired"], %{
               deps(Substrate)
               | leases: ExpiredLeases
             })

    assert receipt.status == :failed
    assert_received {:lease_ttl, 720_000}
    refute_received {:promote, _row}
    assert_received {:lease_release, {:volume, "pgdata"}, "expired-token"}
  end

  defp deps(substrate) do
    %{
      commands: Commands,
      leases: Leases,
      volumes: Volumes,
      transaction: Transaction,
      substrate: substrate
    }
  end
end
