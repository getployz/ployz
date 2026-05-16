defmodule Ployz.Metadata.Machines do
  @moduledoc """
  Narrow metadata API for durable machine lifecycle truth.
  """

  alias Ployz.Metadata.Tables

  @selectable_statuses [:active]

  def put_joining(id, attrs \\ %{}) when is_binary(id) and is_map(attrs) do
    machine =
      attrs
      |> Map.put(:id, id)
      |> Map.put(:status, :joining)
      |> Map.put_new(:inserted_at, now())
      |> Map.put(:updated_at, now())

    Tables.write(:machines, id, machine)
  end

  def activate(id) when is_binary(id) do
    update(id, fn machine ->
      {:ok, Map.merge(machine, %{status: :active, updated_at: now()})}
    end)
  end

  def mark_removed(id) when is_binary(id) do
    update(id, fn machine ->
      {:ok, Map.merge(machine, %{status: :removed, updated_at: now(), removed_at: now()})}
    end)
  end

  def mark_draining(id) when is_binary(id) do
    update(id, fn machine ->
      {:ok, Map.merge(machine, %{status: :draining, updated_at: now()})}
    end)
  end

  def get(id), do: Tables.read(:machines, id)

  def selectable do
    :machines
    |> Tables.all()
    |> Enum.map(fn {_id, machine} -> machine end)
    |> Enum.filter(fn %{status: status} -> status in @selectable_statuses end)
  end

  def active_runtime_members do
    members =
      selectable()
      |> Enum.filter(fn machine ->
        roles = Map.get(machine, :roles, [:runtime])
        :runtime in roles or "runtime" in roles
      end)
      |> Enum.map(&Map.fetch!(&1, :id))

    {:ok, members}
  end

  def removed?(id) do
    case get(id) do
      {:ok, %{status: :removed}} -> true
      {:ok, _machine} -> false
      {:error, :not_found} -> false
    end
  end

  defp update(id, fun) do
    with {:ok, machine} <- Tables.read(:machines, id),
         {:ok, next} <- fun.(machine) do
      Tables.write(:machines, id, next)
    end
  end

  defp now, do: System.system_time(:millisecond)
end
