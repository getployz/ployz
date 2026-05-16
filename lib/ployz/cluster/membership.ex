defmodule Ployz.Cluster.Membership do
  @moduledoc false

  alias Ployz.Cluster.Groups
  alias Ployz.Metadata.Machines

  def runtime_candidates do
    live = Groups.members(Groups.runtime_group()) |> MapSet.new()

    Machines.selectable()
    |> Enum.reject(fn machine -> machine.status == :removed end)
    |> Enum.filter(fn machine ->
      pid = Map.get(machine, :runtime_pid)
      is_pid(pid) and MapSet.member?(live, pid) and Process.alive?(pid)
    end)
  end
end
