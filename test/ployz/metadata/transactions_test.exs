defmodule Ployz.Metadata.TransactionsTest do
  use ExUnit.Case, async: false

  alias Ployz.Metadata.Commands
  alias Ployz.Metadata.Leases
  alias Ployz.Metadata.Revisions
  alias Ployz.Metadata.Routes
  alias Ployz.Metadata.Tables

  setup :reset_mnesia

  test "metadata commit can write revision, service head, route, and receipt atomically" do
    assert {:ok, :ok} =
             Tables.transaction(fn ->
               :ok = Commands.create_running("cmd-1", :deploy, %{role: :local_operator})
               :ok = Revisions.put_revision("web", 1, %{image: "example"})
               :ok = Revisions.put_head("web", 1)
               :ok = Routes.put("web.example", %{service_id: "web", revision: 1})
               :ok = Commands.succeed("cmd-1", %{revision: 1})
             end)

    assert {:ok, %{revision: 1}} = Revisions.get_head("web")
    assert {:ok, %{revision: 1}} = Routes.get("web.example")
    assert {:ok, %{status: :succeeded}} = Commands.get("cmd-1")
  end

  test "failed transaction leaves no partial route or service head" do
    assert {:error, :boom} =
             Tables.transaction(fn ->
               :ok = Revisions.put_head("web", 1)
               :ok = Routes.put("web.example", %{service_id: "web", revision: 1})
               :mnesia.abort(:boom)
             end)

    assert {:error, :not_found} = Revisions.get_head("web")
    assert {:error, :not_found} = Routes.get("web.example")
  end

  test "expired lease can be acquired but unexpired lease cannot be stolen" do
    assert {:ok, {:ok, token}} =
             Tables.transaction(fn -> Leases.acquire({:deploy, "web"}, :owner_a, 100, 1_000) end)

    assert {:ok, {:error, {:lease_held, :owner_a, 1_100}}} =
             Tables.transaction(fn -> Leases.acquire({:deploy, "web"}, :owner_b, 100, 1_050) end)

    assert {:ok, {:ok, new_token}} =
             Tables.transaction(fn -> Leases.acquire({:deploy, "web"}, :owner_b, 100, 1_101) end)

    refute new_token == token
  end

  test "certs store references and drop secret-bearing material" do
    assert {:ok, :ok} =
             Tables.transaction(fn ->
               Ployz.Metadata.Certs.put("web.example", %{
                 cert_ref: "ployz-cert://web.example/1",
                 private_key: "SECRET",
                 pem: "PEM"
               })
             end)

    assert {:ok, cert} = Ployz.Metadata.Certs.get("web.example")
    assert cert.cert_ref == "ployz-cert://web.example/1"
    refute Map.has_key?(cert, :private_key)
    refute Map.has_key?(cert, :pem)
  end

  defp reset_mnesia(context), do: Ployz.TestSupport.Mnesia.reset!(context)
end
