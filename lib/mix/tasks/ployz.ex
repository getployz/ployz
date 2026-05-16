defmodule Mix.Tasks.Ployz do
  @moduledoc """
  Experimental v2 operator surface.

      mix ployz deploy path/to/ployz.yml
      mix ployz machine add node-a
      mix ployz machine remove node-a
      mix ployz cert issue example.com
      mix ployz migrate-volume data --to machine-a
      mix ployz gateway routes
  """

  use Mix.Task

  alias Ployz.Cli.Output

  @shortdoc "Runs experimental Ployz v2 commands"

  @impl Mix.Task
  def run(args) do
    Mix.Task.run("app.start")

    case dispatch(args) do
      {:ok, _payload} = result ->
        result
        |> Output.format(format: :text)
        |> Mix.shell().info()

      {:error, _payload} = result ->
        message = Output.format(result, format: :text)
        Mix.shell().error(message)
        Mix.raise(message)

      {:error, _reason, _receipt} = result ->
        message = Output.format(result, format: :text)
        Mix.shell().error(message)
        Mix.raise(message)
    end
  end

  def dispatch(["deploy", path]) do
    endpoint_dispatch(%{command: :deploy, args: %{manifest_path: path}, opts: []})
  end

  def dispatch(["machine", "add", id]) do
    endpoint_dispatch(%{command: :machine_add, args: %{id: id}, opts: []})
  end

  def dispatch(["machine", "remove", id]) do
    endpoint_dispatch(%{command: :machine_remove, args: %{id: id}, opts: []})
  end

  def dispatch(["cert", "issue", hostname]) do
    endpoint_dispatch(%{command: :cert_issue, args: %{hostname: hostname}, opts: []})
  end

  def dispatch(["migrate-volume", volume, "--to", destination]) do
    endpoint_dispatch(%{
      command: :migrate_volume,
      args: %{volume: volume, destination: destination},
      opts: []
    })
  end

  def dispatch(["gateway", "routes"]) do
    endpoint_dispatch(%{command: :gateway_routes, args: %{}, opts: []})
  end

  def dispatch(["status"]) do
    endpoint_dispatch(%{command: :status, args: %{}, opts: []})
  end

  def dispatch(["manifest", "check", path]) do
    Ployz.Manifest.parse_file(path)
  end

  def dispatch(_args) do
    {:error,
     {:usage,
      "mix ployz machine add <id> | mix ployz machine remove <id> | mix ployz deploy <manifest> | mix ployz cert issue <hostname> | mix ployz migrate-volume <volume> --to <machine> | mix ployz gateway routes | mix ployz status | mix ployz manifest check <manifest>"},
     nil}
  end

  defp endpoint_dispatch(request) do
    endpoint = Application.get_env(:ployz, :command_endpoint, Ployz.CommandEndpoint)
    actor = Application.get_env(:ployz, :cli_actor, %{role: :local_operator})

    if Code.ensure_loaded?(endpoint) and function_exported?(endpoint, :authorize_and_dispatch, 2) do
      endpoint.authorize_and_dispatch(actor, request)
    else
      {:error,
       %{
         audience: :operator,
         phase: :dispatch,
         reason: {:invalid_command_endpoint, endpoint},
         receipt: nil
       }}
    end
  end
end
