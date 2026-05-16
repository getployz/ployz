defmodule Ployz.Status do
  @moduledoc """
  Operator status surface that keeps committed metadata separate from live
  observations.
  """

  alias Ployz.Cluster.Groups
  alias Ployz.Metadata.{Machines, Routes}

  def snapshot do
    {:ok, routes} = Routes.committed()

    {:ok,
     %{
       machines: Machines.selectable(),
       routes: routes,
       live_runtime_members: Groups.members(Groups.runtime_group()),
       freshness: :local
     }}
  end
end
