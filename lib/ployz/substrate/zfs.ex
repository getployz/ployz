defmodule Ployz.Substrate.Zfs do
  @moduledoc """
  Placeholder ZFS helper boundary for the v2 MVP.

  The migration command owns commit semantics now; real send/receive work can be
  wired into this module without changing the durable volume boundary.
  """

  def snapshot(volume, _timeout), do: unavailable(:snapshot, volume)

  def send(%{} = snapshot, _timeout), do: unavailable(:send, snapshot)

  def recv(%{} = stream, destination, _timeout), do: unavailable(:recv, {stream, destination})

  def verify(%{} = transfer, _timeout), do: unavailable(:verify, transfer)

  defp unavailable(op, context) do
    {:error, %{code: :zfs_helper_not_configured, op: op, context: context}}
  end
end
