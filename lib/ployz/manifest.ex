defmodule Ployz.Manifest do
  @moduledoc """
  Tiny native deploy manifest.

  The MVP format is deliberately small YAML-like syntax:

      service: web
      image: ghcr.io/example/web:sha
      instances: 2
      env:
        DATABASE_URL: ployz-secret://prod/database-url
      routes:
        - host: example.com
          path: /
          port: 4000
      volumes:
        - name: data
          mount: /data

  Secret-bearing values must be references such as `ployz-secret://...`; raw
  private keys or certificate PEM material are rejected.
  """

  alias Ployz.Manifest.{Parser, Validator}

  defstruct service: nil,
            image: nil,
            command: nil,
            instances: 1,
            env: %{},
            routes: [],
            volumes: [],
            metadata: %{}

  @type route :: %{host: String.t(), path: String.t(), port: pos_integer()}
  @type volume :: %{name: String.t(), mount: String.t() | nil}

  @type t :: %__MODULE__{
          service: String.t(),
          image: String.t(),
          command: String.t() | nil,
          instances: pos_integer(),
          env: %{optional(String.t()) => String.t()},
          routes: [route()],
          volumes: [volume()],
          metadata: map()
        }

  @spec parse_file(Path.t()) :: {:ok, t()} | {:error, term()}
  def parse_file(path) do
    with {:ok, content} <- File.read(path) do
      parse_string(content)
    end
  end

  @spec parse_string(String.t()) :: {:ok, t()} | {:error, term()}
  def parse_string(content) when is_binary(content) do
    with {:ok, raw} <- Parser.parse(content),
         {:ok, manifest} <- Validator.validate(raw) do
      {:ok, manifest}
    end
  end
end
