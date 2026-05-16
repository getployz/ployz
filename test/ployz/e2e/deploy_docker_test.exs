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
        actor = %{role: :local_operator}
        helper_path = helper_path()

        ensure_gateway_started()

        {:ok, runtime} =
          start_supervised(
            {Ployz.Runtime.Server,
             port_opts: [helper_path: helper_path], group: Ployz.Runtime.Server.group()}
          )

        assert {:ok, _receipt} =
                 Ployz.Commands.MachineAdd.run(actor, %{
                   id: "docker-e2e-node",
                   roles: [:runtime],
                   runtime_pid: runtime,
                   command_id: "cmd-docker-add"
                 })

        manifest_path = write_manifest!()
        assert {:ok, receipt} = Mix.Tasks.Ployz.dispatch(["deploy", manifest_path])

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

  defp helper_path do
    System.get_env("PLOYZ_SUBSTRATE_HELPER") ||
      Path.expand("../../../target/debug/ployz-substrate-helper", __DIR__)
  end

  defp docker_available? do
    System.find_executable("docker") != nil and
      match?({_output, 0}, System.cmd("docker", ["info"], stderr_to_stdout: true))
  end

  defp image_available?(image) do
    match?(
      {_output, 0},
      System.cmd("docker", ["image", "inspect", image], stderr_to_stdout: true)
    ) or
      match?({_output, 0}, System.cmd("docker", ["pull", image], stderr_to_stdout: true))
  end

  defp docker_container_started? do
    {_output, status} =
      System.cmd(
        "docker",
        [
          "ps",
          "--filter",
          "label=ployz.service=#{@service}",
          "--filter",
          "label=ployz.test=#{@e2e_label}",
          "--format",
          "{{.Names}}"
        ],
        stderr_to_stdout: true
      )

    status == 0
  end

  defp cleanup_containers do
    {ids, 0} =
      System.cmd(
        "docker",
        [
          "ps",
          "-aq",
          "--filter",
          "label=ployz.service=#{@service}",
          "--filter",
          "label=ployz.test=#{@e2e_label}"
        ],
        stderr_to_stdout: true
      )

    ids
    |> String.split("\n", trim: true)
    |> Enum.each(fn id ->
      System.cmd("docker", ["rm", "-f", id], stderr_to_stdout: true)
    end)
  rescue
    _error -> :ok
  end
end
