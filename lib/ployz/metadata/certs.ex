defmodule Ployz.Metadata.Certs do
  @moduledoc false

  alias Ployz.Metadata.Tables
  alias Ployz.Redactor

  @pem_markers ["-----BEGIN", "PRIVATE KEY", "CERTIFICATE-----"]

  def put(hostname, cert) when is_binary(hostname) and is_map(cert) do
    cert = Redactor.redact(cert)

    cond do
      contains_pem_material?(cert) ->
        {:error, :secret_material_not_allowed}

      is_binary(cert[:cert_ref]) ->
        Tables.write(:certs, hostname, Map.put(cert, :hostname, hostname))

      is_binary(cert["cert_ref"]) ->
        Tables.write(
          :certs,
          hostname,
          cert |> atomize_allowed_keys() |> Map.put(:hostname, hostname)
        )

      true ->
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
    case Tables.all(:certs) do
      {:error, reason} ->
        {:error, reason}

      rows ->
        refs =
          rows
          |> Enum.map(fn {hostname, cert} ->
            {hostname, Map.take(cert, [:cert_ref, :chain_ref, :revision, :expires_at])}
          end)
          |> Map.new()

        {:ok, refs}
    end
  end

  defp atomize_allowed_keys(cert) do
    Map.new(cert, fn
      {"cert_ref", value} -> {:cert_ref, value}
      {"chain_ref", value} -> {:chain_ref, value}
      {"revision", value} -> {:revision, value}
      {"expires_at", value} -> {:expires_at, value}
      {"issued_at", value} -> {:issued_at, value}
      {key, value} -> {key, value}
    end)
  end

  defp contains_pem_material?(value) do
    value
    |> inspect()
    |> then(fn inspected -> Enum.any?(@pem_markers, &String.contains?(inspected, &1)) end)
  end
end
