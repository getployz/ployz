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

    def send_receive(snapshot, "machine-b", _timeout),
      do: {:ok, Map.put(snapshot, :dataset_ref, "zfs://machine-b/pgdata@3")}

    def verify_destination(_transfer, _timeout), do: :ok
  end

  defmodule FailingSubstrate do
    def snapshot("pgdata", _timeout), do: {:ok, %{snapshot_ref: "zfs://snap"}}

    def send_receive(snapshot, "machine-b", _timeout),
      do: {:ok, Map.put(snapshot, :dataset_ref, "zfs://machine-b/pgdata@3")}

    def verify_destination(_transfer, _timeout), do: {:error, :checksum_mismatch}
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
               deps(FailingSubstrate)
             )

    assert receipt.status == :failed
    refute_received {:promote, _row}
    assert_received {:command_finish, %{id: "cmd-migrate-fail", status: :failed}}
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
