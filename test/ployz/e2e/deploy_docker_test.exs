defmodule Ployz.E2E.DeployDockerTest do
  use ExUnit.Case, async: false

  @moduletag :docker
  @image "busybox:1.36"
  @service "docker-e2e"
  @e2e_label "docker-deploy"

  setup context do
    dir =
      Path.join(
        System.tmp_dir!(),
        "ployz-e2e-mnesia-#{inspect(context.test)}-#{System.unique_integer([:positive])}"
      )

    Ployz.Metadata.Schema.reset_for_test!(dir)
    cleanup_containers()

    on_exit(fn ->
      cleanup_containers()
      :mnesia.stop()
      File.rm_rf!(dir)
    end)

    :ok
  end

  test "mix ployz deploy runs through RuntimeServer, Rust helper, Docker, and gateway projection" do
    cond do
      not docker_available?() ->
        IO.puts(
          "Skipping Docker-backed v2 e2e: Docker is not available or the daemon is not reachable"
        )

        assert true

      not helper_available?() ->
        IO.puts("Skipping Docker-backed v2 e2e: ployz-substrate-helper binary is unavailable")
        assert true

      not image_available?(@image) ->
        IO.puts(
          "Skipping Docker-backed v2 e2e: #{@image} is not available and could not be pulled"
        )

        assert true

      true ->
        helper_path = helper_path()

        Application.put_env(:ployz, :substrate_helper_path, helper_path)
        ensure_gateway_started()
        ensure_runtime_started()

        assert {:ok, %{receipt: add_receipt}} =
                 Mix.Tasks.Ployz.dispatch(["machine", "add", "docker-e2e-node"])

        assert add_receipt.status == :committed

        manifest_path = write_manifest!()
        assert {:ok, %{receipt: receipt}} = Mix.Tasks.Ployz.dispatch(["deploy", manifest_path])

        assert receipt.status == :committed
        assert receipt.service == @service
        assert receipt.revision == 1
        assert receipt.gateway.status == :fresh

        assert {:ok, [route]} = Ployz.Metadata.Routes.committed()
        assert route.host == "docker-e2e.local"
        assert route.service == @service
        assert route.revision == 1

        assert docker_container_started?()
    end
  end

  defp write_manifest! do
    path =
      Path.join(System.tmp_dir!(), "ployz-docker-e2e-#{System.unique_integer([:positive])}.yml")

    File.write!(path, """
    service: #{@service}
    image: #{@image}
    command: sleep 60
    metadata:
      e2e: #{@e2e_label}
    routes:
      - host: docker-e2e.local
        path: /
        port: 80
    """)

    path
  end

  defp helper_available?, do: File.exists?(helper_path())

  defp ensure_gateway_started do
    case Process.whereis(Ployz.Gateway.Projection) do
      nil -> start_supervised(Ployz.Gateway.Projection)
      pid when is_pid(pid) -> {:ok, pid}
    end
  end

  defp ensure_runtime_started do
    case Process.whereis(Ployz.Runtime.Server) do
      nil -> start_supervised({Ployz.Runtime.Server, name: Ployz.Runtime.Server})
      pid when is_pid(pid) -> {:ok, pid}
    end
  end

  defp helper_path do
    System.get_env("PLOYZ_SUBSTRATE_HELPER") ||
      Path.expand("../../../target/debug/ployz-substrate-helper", __DIR__)
  end

  defp docker_available? do
    System.find_executable("docker") != nil and
      match?({_output, 0}, docker_cmd(["info"], 15_000))
  end

  defp image_available?(image) do
    match?(
      {_output, 0},
      docker_cmd(["image", "inspect", image], 15_000)
    ) or
      match?({_output, 0}, docker_cmd(["pull", image], 60_000))
  end

  defp docker_container_started? do
    case docker_cmd([
           "ps",
           "--filter",
           "label=ployz.service=#{@service}",
           "--filter",
           "label=ployz.test=#{@e2e_label}",
           "--format",
           "{{.Names}}"
         ]) do
      {output, 0} -> output |> String.split("\n", trim: true) |> Enum.any?()
      {_output, _status} -> false
    end
  end

  defp cleanup_containers do
    {ids, _status} =
      docker_cmd([
        "ps",
        "-aq",
        "--filter",
        "label=ployz.service=#{@service}",
        "--filter",
        "label=ployz.test=#{@e2e_label}"
      ])

    ids
    |> String.split("\n", trim: true)
    |> Enum.each(fn id ->
      docker_cmd(["rm", "-f", id])
    end)
  rescue
    _error -> :ok
  end

  defp docker_cmd(args, timeout \\ 30_000) do
    task = Task.async(fn -> System.cmd("docker", args, stderr_to_stdout: true) end)

    case Task.yield(task, timeout) || Task.shutdown(task, :brutal_kill) do
      {:ok, result} -> result
      nil -> {"", 124}
    end
  rescue
    _error ->
      {"", 124}
  end
end
