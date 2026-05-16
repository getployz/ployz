defmodule Ployz.Cluster.Groups do
  @moduledoc """
  Thin helpers around `:pg` live role groups.
  """

  @runtime {:ployz, :runtime}
  @metadata {:ployz, :metadata}

  def runtime_group, do: @runtime
  def metadata_group, do: @metadata

  def join(group, pid \\ self()) when is_pid(pid) do
    :pg.join(group, pid)
  end

  def leave(group, pid \\ self()) when is_pid(pid) do
    :pg.leave(group, pid)
  end

  def members(group) do
    :pg.get_members(group)
  end

  def member?(group, pid) when is_pid(pid) do
    pid in members(group)
  end

  def has_live_member?(group) do
    Enum.any?(members(group), &Process.alive?/1)
  end
end
