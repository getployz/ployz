defmodule Ployz.Cluster.Distribution do
  @moduledoc false

  import Bitwise

  def validate_config(:dev, _opts), do: :ok
  def validate_config("dev", _opts), do: :ok

  def validate_config(:clustered, opts) do
    cookie_path = Keyword.get(opts, :cookie_path)
    bind_address = Keyword.get(opts, :bind_address)

    cond do
      !is_binary(bind_address) or bind_address == "" ->
        {:error, :missing_bind_address}

      !is_binary(cookie_path) or cookie_path == "" ->
        {:error, :missing_cookie_path}

      !File.exists?(cookie_path) ->
        {:error, :missing_cookie_file}

      insecure_cookie_mode?(cookie_path) ->
        {:error, :insecure_cookie_file}

      true ->
        :ok
    end
  end

  defp insecure_cookie_mode?(path) do
    mode = File.stat!(path).mode &&& 0o777
    (mode &&& 0o077) != 0
  end
end
