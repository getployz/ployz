defmodule Ployz.Metadata.Revisions do
  @moduledoc false

  alias Ployz.Metadata.Tables

  def next_for_service(service_id) do
    case get_head(service_id) do
      {:ok, %{revision: revision}} -> {:ok, revision + 1}
      {:error, :not_found} -> {:ok, 1}
      {:error, reason} -> {:error, reason}
    end
  end

  def put(%{service: service_id, revision: revision} = metadata) do
    put_revision(service_id, revision, metadata)
  end

  def put_revision(service_id, revision, metadata) do
    Tables.write(
      :revisions,
      {service_id, revision},
      Map.merge(metadata, %{service_id: service_id, revision: revision})
    )
  end

  def get_revision(service_id, revision), do: Tables.read(:revisions, {service_id, revision})

  def put_head(service_id, revision) do
    Tables.write(:service_heads, service_id, %{service_id: service_id, revision: revision})
  end

  def get_head(service_id), do: Tables.read(:service_heads, service_id)
end
