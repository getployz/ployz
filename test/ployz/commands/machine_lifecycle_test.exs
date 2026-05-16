defmodule Ployz.Commands.MachineLifecycleTest do
  use ExUnit.Case, async: false

  alias Ployz.Cluster.Groups
  alias Ployz.Commands.MachineAdd
  alias Ployz.Commands.MachineRemove
  alias Ployz.Metadata.Commands
  alias Ployz.Metadata.Machines

  setup :reset_mnesia

  test "machine add records joining then active after readiness" do
    actor = %{role: :local_operator}
    join_runtime_group()

    assert {:ok, %{status: :committed, result: %{status: :active}}} =
             MachineAdd.run(actor, %{id: "node-1", command_id: "cmd-1", runtime_pid: self()})

    assert {:ok, %{status: :active, runtime_pid: pid}} = Machines.get("node-1")
    assert pid == self()
    assert {:ok, %{status: :committed}} = Commands.get("cmd-1")
  end

  test "machine add failure leaves machine non-selectable and receipt visible" do
    actor = %{role: :local_operator}
    readiness = fn _args -> {:error, :runtime_not_ready} end

    assert {:error, %{status: :failed}} =
             MachineAdd.run(actor, %{id: "node-1", command_id: "cmd-1"}, readiness: readiness)

    assert {:ok, %{status: :joining}} = Machines.get("node-1")
    assert [] = Machines.selectable()
    assert {:ok, %{last_error: %{code: :runtime_not_ready}}} = Commands.get("cmd-1")
  end

  test "machine remove marks removed and excludes future candidates" do
    actor = %{role: :local_operator}
    join_runtime_group()

    assert {:ok, _receipt} =
             MachineAdd.run(actor, %{id: "node-1", command_id: "cmd-1", runtime_pid: self()})

    assert {:ok, %{status: :committed}} =
             MachineRemove.run(actor, %{id: "node-1", command_id: "cmd-2"})

    assert {:ok, %{status: :removed}} = Machines.get("node-1")
    assert [] = Machines.selectable()
  end

  test "runtime candidates require active metadata and live pg membership" do
    group = Groups.runtime_group()
    join_runtime_group()

    actor = %{role: :local_operator}

    assert {:ok, _receipt} =
             MachineAdd.run(actor, %{id: "node-1", runtime_pid: self(), command_id: "cmd-1"})

    assert [%{id: "node-1"}] = Ployz.Cluster.Membership.runtime_candidates()

    assert :ok = Groups.leave(group, self())
    assert [] = Ployz.Cluster.Membership.runtime_candidates()
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)

  defp join_runtime_group do
    group = Groups.runtime_group()

    case Groups.join(group, self()) do
      :ok -> :ok
      {:error, {:already_joined, _pid}} -> :ok
    end
  end
end
