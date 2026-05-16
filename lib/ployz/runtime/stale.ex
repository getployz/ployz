defmodule Ployz.Runtime.Stale do
  @moduledoc """
  Pure runtime observation classifier.

  This module does not mutate metadata or runtime state. It labels inspected
  substrate resources against committed service, volume, and machine metadata.
  """

  @classifications [:current, :stale, :unknown, :removed_machine]

  @type classification :: :current | :stale | :unknown | :removed_machine
  @type resource :: map()
  @type committed :: %{
          optional(:service_heads) => map(),
          optional(:volumes) => map(),
          optional(:machines) => map()
        }

  @spec classifications() :: [classification()]
  def classifications, do: @classifications

  @spec classify([resource()] | resource(), committed()) :: [resource()]
  def classify(resources, committed) when is_list(resources) and is_map(committed) do
    Enum.map(resources, &classify_one(&1, committed))
  end

  def classify(resource, committed) when is_map(resource) and is_map(committed) do
    [classify_one(resource, committed)]
  end

  @spec classify_one(resource(), committed()) :: resource()
  def classify_one(resource, committed) when is_map(resource) and is_map(committed) do
    classification =
      cond do
        malformed?(resource) ->
          :unknown

        removed_machine?(resource, committed) ->
          :removed_machine

        current_service?(resource, committed) or current_volume?(resource, committed) ->
          :current

        known_service?(resource, committed) or known_volume?(resource, committed) ->
          :stale

        true ->
          :unknown
      end

    Map.put(resource, :classification, classification)
  end

  def classify_one(_resource, _committed), do: %{classification: :unknown}

  defp malformed?(resource) do
    not is_map(resource) or blank?(field(resource, :kind))
  end

  defp removed_machine?(resource, committed) do
    machine = field(resource, :machine)

    case machine_status(machine, committed) do
      "removed" -> true
      :removed -> true
      _status -> false
    end
  end

  defp current_service?(resource, committed) do
    service = field(resource, :service)
    revision = field(resource, :revision)

    not blank?(service) and not blank?(revision) and
      revision == get_in_string(committed, [:service_heads, service])
  end

  defp known_service?(resource, committed) do
    service = field(resource, :service)
    not blank?(service) and not is_nil(get_in_string(committed, [:service_heads, service]))
  end

  defp current_volume?(resource, committed) do
    volume = field(resource, :volume)
    generation = field(resource, :generation)

    not blank?(volume) and not blank?(generation) and
      generation == get_in_string(committed, [:volumes, volume])
  end

  defp known_volume?(resource, committed) do
    volume = field(resource, :volume)
    not blank?(volume) and not is_nil(get_in_string(committed, [:volumes, volume]))
  end

  defp machine_status(machine, committed) when not is_nil(machine) do
    get_in_string(committed, [:machines, machine])
  end

  defp machine_status(_machine, _committed), do: nil

  defp field(map, key) do
    Map.get(map, key) || Map.get(map, Atom.to_string(key))
  end

  defp get_in_string(map, [first, second]) do
    nested = Map.get(map, first) || Map.get(map, Atom.to_string(first)) || %{}
    Map.get(nested, second) || Map.get(nested, to_string(second))
  end

  defp blank?(nil), do: true
  defp blank?(""), do: true
  defp blank?(_value), do: false
end
