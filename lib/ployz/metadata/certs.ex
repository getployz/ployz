defmodule Ployz.Metadata.Certs do
  @moduledoc false

  alias Ployz.Metadata.Tables

  @forbidden_keys [:private_key, :pem, :account_key, :secret]

  def put(hostname, cert) when is_binary(hostname) and is_map(cert) do
    cert = Map.drop(cert, @forbidden_keys)

    if is_binary(cert[:cert_ref]) do
      Tables.write(:certs, hostname, Map.put(cert, :hostname, hostname))
    else
      {:error, :missing_cert_ref}
    end
  end

  def get(hostname), do: Tables.read(:certs, hostname)

  def next_revision(hostname) do
    case get(hostname) do
      {:ok, %{revision: revision}} -> {:ok, revision + 1}
      {:error, :not_found} -> {:ok, 1}
      {:error, reason} -> {:error, reason}
    end
  end

  def active_refs do
    refs =
      :certs
      |> Tables.all()
      |> Enum.map(fn {hostname, cert} ->
        {hostname, Map.take(cert, [:cert_ref, :chain_ref, :revision, :expires_at])}
      end)
      |> Map.new()

    {:ok, refs}
  end
end
