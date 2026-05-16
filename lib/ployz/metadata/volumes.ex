defmodule Ployz.Metadata.Volumes do
  @moduledoc false

  alias Ployz.Metadata.Tables

  def put(volume_id, volume) when is_binary(volume_id) and is_map(volume) do
    Tables.write(:volumes, volume_id, Map.put(volume, :id, volume_id))
  end

  def get(volume_id), do: Tables.read(:volumes, volume_id)

  def next_generation(volume_id) do
    case get(volume_id) do
      {:ok, %{generation: generation}} -> {:ok, generation + 1}
      {:error, :not_found} -> {:ok, 1}
      {:error, reason} -> {:error, reason}
    end
  end

  def promote_generation(%{volume: volume_id} = volume), do: put(volume_id, volume)
end
