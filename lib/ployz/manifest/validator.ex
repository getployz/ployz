defmodule Ployz.Manifest.Validator do
  @moduledoc false

  alias Ployz.Manifest

  @service_name ~r/^[a-z][a-z0-9-]{0,62}$/
  @secret_ref ~r/^ployz-secret:\/\/[A-Za-z0-9._\/-]+$/
  @pem_markers ["-----BEGIN", "PRIVATE KEY", "CERTIFICATE-----"]

  @spec validate(map()) :: {:ok, Manifest.t()} | {:error, term()}
  def validate(raw) when is_map(raw) do
    with {:ok, service} <- required_string(raw, "service"),
         :ok <- valid_service_name(service),
         {:ok, image} <- required_string(raw, "image"),
         {:ok, instances} <- instances(raw),
         {:ok, env} <- env(raw),
         {:ok, routes} <- routes(raw),
         {:ok, volumes} <- volumes(raw),
         :ok <- reject_secret_material(raw) do
      {:ok,
       %Manifest{
         service: service,
         image: image,
         command: optional_string(raw, "command"),
         instances: instances,
         env: env,
         routes: routes,
         volumes: volumes,
         metadata: Map.get(raw, "metadata", %{})
       }}
    end
  end

  defp required_string(raw, key) do
    case Map.get(raw, key) do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, {:manifest_invalid, key, :required_string}}
    end
  end

  defp optional_string(raw, key) do
    case Map.get(raw, key) do
      value when is_binary(value) and value != "" -> value
      _ -> nil
    end
  end

  defp valid_service_name(service) do
    if Regex.match?(@service_name, service) do
      :ok
    else
      {:error, {:manifest_invalid, "service", :invalid_name}}
    end
  end

  defp instances(raw) do
    case Map.get(raw, "instances", 1) do
      value when is_integer(value) and value > 0 -> {:ok, value}
      _ -> {:error, {:manifest_invalid, "instances", :positive_integer}}
    end
  end

  defp env(raw) do
    raw
    |> Map.get("env", %{})
    |> case do
      env when is_map(env) ->
        invalid =
          Enum.find(env, fn
            {key, value} when is_binary(key) and is_binary(value) ->
              not Regex.match?(@secret_ref, value)

            _ ->
              true
          end)

        case invalid do
          nil ->
            {:ok, env}

          {key, _value} ->
            {:error, {:manifest_invalid, "env.#{key}", :env_must_be_secret_reference}}
        end

      _ ->
        {:error, {:manifest_invalid, "env", :map_required}}
    end
  end

  defp routes(raw) do
    raw
    |> Map.get("routes", [])
    |> list_of_maps("routes")
    |> case do
      {:ok, routes} ->
        routes
        |> Enum.with_index()
        |> Enum.reduce_while({:ok, []}, fn {route, index}, {:ok, acc} ->
          with {:ok, host} <- route_required(route, index, "host"),
               {:ok, port} <- route_port(route, index) do
            path = Map.get(route, "path", "/")
            {:cont, {:ok, [%{host: host, path: path, port: port} | acc]}}
          else
            {:error, _reason} = error -> {:halt, error}
          end
        end)
        |> case do
          {:ok, routes} -> reject_duplicate_routes(Enum.reverse(routes))
          error -> error
        end

      error ->
        error
    end
  end

  defp volumes(raw) do
    raw
    |> Map.get("volumes", [])
    |> list_of_maps("volumes")
    |> case do
      {:ok, volumes} ->
        volumes
        |> Enum.with_index()
        |> Enum.reduce_while({:ok, []}, fn {volume, index}, {:ok, acc} ->
          case route_required(volume, index, "name") do
            {:ok, name} ->
              {:cont, {:ok, [%{name: name, mount: Map.get(volume, "mount")} | acc]}}

            {:error, _reason} = error ->
              {:halt, error}
          end
        end)
        |> case do
          {:ok, volumes} -> {:ok, Enum.reverse(volumes)}
          error -> error
        end

      error ->
        error
    end
  end

  defp list_of_maps(values, key) when is_list(values) do
    if Enum.all?(values, &is_map/1) do
      {:ok, values}
    else
      {:error, {:manifest_invalid, key, :list_of_maps_required}}
    end
  end

  defp list_of_maps(_values, key), do: {:error, {:manifest_invalid, key, :list_required}}

  defp reject_duplicate_routes(routes) do
    duplicate =
      routes
      |> Enum.map(fn route -> {route.host, route.path} end)
      |> Enum.find(fn key ->
        Enum.count(routes, fn route -> {route.host, route.path} == key end) > 1
      end)

    case duplicate do
      nil -> {:ok, routes}
      {host, path} -> {:error, {:manifest_invalid, "routes", {:duplicate_route, host, path}}}
    end
  end

  defp route_required(map, index, key) do
    case Map.get(map, key) do
      value when is_binary(value) and value != "" -> {:ok, value}
      _ -> {:error, {:manifest_invalid, "#{key}[#{index}]", :required_string}}
    end
  end

  defp route_port(map, index) do
    case Map.get(map, "port") do
      value when is_integer(value) and value > 0 and value < 65_536 -> {:ok, value}
      _ -> {:error, {:manifest_invalid, "routes[#{index}].port", :valid_port_required}}
    end
  end

  defp reject_secret_material(raw) do
    raw
    |> inspect()
    |> then(fn inspected ->
      if Enum.any?(@pem_markers, &String.contains?(inspected, &1)) do
        {:error, {:manifest_invalid, :secret_material, :not_allowed}}
      else
        :ok
      end
    end)
  end
end
