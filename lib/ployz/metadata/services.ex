defmodule Ployz.Metadata.Services do
  @moduledoc false

  alias Ployz.Metadata.Tables

  def put(service_id, service) when is_binary(service_id) and is_map(service) do
    Tables.write(:services, service_id, Map.put(service, :id, service_id))
  end

  def get(service_id), do: Tables.read(:services, service_id)

  def put_head(service_id, revision) do
    Tables.write(:service_heads, service_id, %{service_id: service_id, revision: revision})
  end
end
