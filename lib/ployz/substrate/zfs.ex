defmodule Ployz.Substrate.Zfs do
  @moduledoc """
  Placeholder ZFS helper boundary for the v2 MVP.

  The migration command owns commit semantics now; real send/receive work can be
  wired into this module without changing the durable volume boundary.
  """

  def snapshot(volume, _timeout), do: unavailable(:snapshot, volume)

  def send_receive(%{} = snapshot, destination, _timeout),
    do: unavailable(:send_receive, {snapshot, destination})

  def verify_destination(%{} = transfer, _timeout), do: unavailable(:verify_destination, transfer)

  defp unavailable(op, context) do
    {:error, %{code: :zfs_helper_not_configured, op: op, context: context}}
  end
end
