defmodule Ployz.Commands.DeployTest do
  use ExUnit.Case, async: false

  alias Ployz.Commands.Deploy
  alias Ployz.Manifest

  defmodule Commands do
    def create(receipt), do: send_event({:command_create, receipt})
    def finish(receipt), do: send_event({:command_finish, receipt})

    defp send_event(event) do
      send(Process.get(:test_pid), event)
      :ok
    end
  end

  defmodule Leases do
    def acquire(key, _owner, _ttl), do: {:ok, %{key: key, token: "lease-token"}}
    def assert_current(_key, "lease-token"), do: :ok

    def release(key, token) do
      send(Process.get(:test_pid), {:lease_release, key, token})
      :ok
    end
  end

  defmodule ExpiredLeases do
    def acquire(key, _owner, _ttl), do: {:ok, %{key: key, token: "expired-token"}}
    def assert_current(_key, "expired-token"), do: {:error, {:lease_expired, 1234}}

    def release(key, token) do
      send(Process.get(:test_pid), {:lease_release, key, token})
      :ok
    end
  end

  defmodule Machines do
    def active_runtime_members, do: {:ok, [:node_a, :node_b]}
  end

  defmodule Runtime do
    def reachable_members(members, _timeout), do: {:ok, members}

    def start_and_probe(members, %Manifest{}, _timeout),
      do: {:ok, %{started: members, probed: true}}
  end

  defmodule RuntimeIdentity do
    def reachable_members(members, timeout),
      do: Ployz.Runtime.Server.reachable_members(members, timeout)

    def start_and_probe(members, %Manifest{}, _timeout),
      do: {:ok, %{started: members, probed: true}}
  end

  defmodule ExplodingRuntime do
    def reachable_members(members, _timeout), do: {:ok, members}
    def start_and_probe(_members, %Manifest{}, _timeout), do: raise("runtime boom")
  end

  defmodule Transaction do
    def transaction(fun), do: {:ok, fun.()}
  end

  defmodule Revisions do
    def next_for_service("web"), do: {:ok, 7}

    def put(row) do
      send(Process.get(:test_pid), {:revision_put, row})
      :ok
    end
  end

  defmodule Services do
    def put_head(service, revision) do
      send(Process.get(:test_pid), {:head_put, service, revision})
      :ok
    end
  end

  defmodule Routes do
    def replace_for_service(service, revision, routes) do
      send(Process.get(:test_pid), {:routes_replace, service, revision, routes})
      :ok
    end
  end

  defmodule Gateway do
    def refresh, do: {:error, :gateway_down}
  end

  setup do
    Process.put(:test_pid, self())
    :ok
  end

  test "commits deploy revision, service head, routes, and receipt in one transaction" do
    manifest = %Manifest{
      service: "web",
      image: "web:latest",
      instances: 1,
      routes: [%{host: "example.com", path: "/", port: 4000}]
    }

    assert {:ok, receipt} =
             Deploy.run_manifest(manifest, [command_id: "cmd-1"], deps())

    assert receipt.status == :committed
    assert receipt.revision == 7
    assert receipt.members == [:node_a]
    assert receipt.gateway.status == :last_good_preserved

    assert_received {:command_create, %{id: "cmd-1", status: :running}}
    assert_received {:revision_put, %{service: "web", revision: 7}}
    assert_received {:head_put, "web", 7}
    assert_received {:routes_replace, "web", 7, [%{host: "example.com", path: "/", port: 4000}]}
    assert_received {:command_finish, %{id: "cmd-1", status: :committed, revision: 7}}
    assert_received {:lease_release, {:deploy, "web"}, "lease-token"}
  end

  test "fails visibly before commit when reachable active members are insufficient" do
    manifest = %Manifest{service: "web", image: "web:latest", instances: 3}

    assert {:error, {:no_reachable_runtime_members, [required: 3, available: 2]}, receipt} =
             Deploy.run_manifest(manifest, [command_id: "cmd-2"], deps())

    assert receipt.status == :failed
    assert_received {:command_finish, %{id: "cmd-2", status: :failed}}
    refute_received {:revision_put, _row}
    refute_received {:head_put, _service, _revision}
    refute_received {:routes_replace, _service, _revision, _routes}
    assert_received {:lease_release, {:deploy, "web"}, "lease-token"}
  end

  test "runtime crashes after lease acquisition release the lease and finish a failed receipt" do
    manifest = %Manifest{service: "web", image: "web:latest", instances: 1}

    assert {:error, %{code: :command_crashed}, receipt} =
             Deploy.run_manifest(manifest, [command_id: "cmd-crash"], %{
               deps()
               | runtime: ExplodingRuntime
             })

    assert receipt.status == :failed
    assert_received {:lease_release, {:deploy, "web"}, "lease-token"}
    assert_received {:command_finish, %{id: "cmd-crash", status: :failed}}
  end

  test "does not commit when the deploy lease expired before commit" do
    manifest = %Manifest{service: "web", image: "web:latest", instances: 1}

    assert {:error, %{code: :command_threw}, receipt} =
             Deploy.run_manifest(manifest, [command_id: "cmd-expired"], %{
               deps()
               | leases: ExpiredLeases
             })

    assert receipt.status == :failed
    refute_received {:revision_put, _row}
    assert_received {:lease_release, {:deploy, "web"}, "expired-token"}
  end

  test "real metadata and pg membership exclude offline, draining, and removed machines" do
    Ployz.TestSupport.Mnesia.reset!(%{test: :deploy_candidate_filter})

    live_runtime = self()
    offline_runtime = spawn(fn -> Process.sleep(:infinity) end)
    draining_runtime = spawn(fn -> Process.sleep(:infinity) end)
    removed_runtime = spawn(fn -> Process.sleep(:infinity) end)

    group = Ployz.Runtime.Server.group()
    :ok = :pg.join(group, live_runtime)
    :ok = :pg.join(group, draining_runtime)
    :ok = :pg.join(group, removed_runtime)

    :ok =
      Ployz.Metadata.Machines.put_joining("active-live", %{
        roles: [:runtime],
        runtime_pid: live_runtime
      })

    :ok = Ployz.Metadata.Machines.activate("active-live")

    :ok =
      Ployz.Metadata.Machines.put_joining("active-offline", %{
        roles: [:runtime],
        runtime_pid: offline_runtime
      })

    :ok = Ployz.Metadata.Machines.activate("active-offline")

    :ok =
      Ployz.Metadata.Machines.put_joining("draining", %{
        roles: [:runtime],
        runtime_pid: draining_runtime
      })

    :ok = Ployz.Metadata.Machines.activate("draining")
    :ok = Ployz.Metadata.Machines.mark_draining("draining")

    :ok =
      Ployz.Metadata.Machines.put_joining("removed", %{
        roles: [:runtime],
        runtime_pid: removed_runtime
      })

    :ok = Ployz.Metadata.Machines.activate("removed")
    :ok = Ployz.Metadata.Machines.mark_removed("removed")

    manifest = %Manifest{service: "web", image: "web:latest", instances: 1}

    assert {:ok, receipt} =
             Deploy.run_manifest(manifest, [command_id: "cmd-real-candidates"], %{
               deps()
               | machines: Ployz.Metadata.Machines,
                 runtime: RuntimeIdentity,
                 commands: Ployz.Metadata.Commands,
                 leases: Ployz.Metadata.Leases,
                 transaction: Ployz.Metadata.Tables,
                 revisions: Ployz.Metadata.Revisions,
                 services: Ployz.Metadata.Services,
                 routes: Ployz.Metadata.Routes
             })

    assert receipt.members == [live_runtime]

    Process.exit(offline_runtime, :kill)
    Process.exit(draining_runtime, :kill)
    Process.exit(removed_runtime, :kill)
  end

  defp deps do
    %{
      commands: Commands,
      leases: Leases,
      machines: Machines,
      runtime: Runtime,
      transaction: Transaction,
      revisions: Revisions,
      services: Services,
      routes: Routes,
      gateway: Gateway
    }
  end
end
