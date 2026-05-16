defmodule Ployz.Commands.AcmeTest do
  use ExUnit.Case, async: false

  alias Ployz.Commands.Acme

  defmodule Commands do
    def create(receipt) do
      send(Process.get(:test_pid), {:command_create, receipt})
      :ok
    end

    def finish(receipt) do
      send(Process.get(:test_pid), {:command_finish, receipt})
      :ok
    end
  end

  defmodule Leases do
    def acquire(key, _owner, _ttl), do: {:ok, %{key: key, token: "lease-token"}}

    def release(key, token) do
      send(Process.get(:test_pid), {:lease_release, key, token})
      :ok
    end
  end

  defmodule Transaction do
    def transaction(fun), do: {:ok, fun.()}
  end

  defmodule Certs do
    def next_revision("example.com"), do: {:ok, 2}

    def put(row) do
      send(Process.get(:test_pid), {:cert_put, row})
      :ok
    end
  end

  defmodule Issuer do
    def issue("example.com", _timeout) do
      {:ok,
       %{
         cert_ref: "ployz-cert://example.com/2",
         chain_ref: "ployz-cert://example.com/2/chain",
         expires_at: 1_800_000_000
       }}
    end
  end

  defmodule LeakyIssuer do
    def issue("example.com", _timeout) do
      {:ok, %{cert_ref: "ployz-cert://example.com/2", pem: "-----BEGIN PRIVATE KEY-----"}}
    end
  end

  setup do
    Process.put(:test_pid, self())
    :ok
  end

  test "commits only cert refs after successful issuance" do
    assert {:ok, receipt} = Acme.issue("example.com", [command_id: "cmd-cert"], deps(Issuer))

    assert receipt.status == :committed
    assert receipt.cert_ref == "ployz-cert://example.com/2"

    assert_received {:cert_put,
                     %{
                       hostname: "example.com",
                       revision: 2,
                       cert_ref: "ployz-cert://example.com/2"
                     }}

    assert_received {:command_finish, %{id: "cmd-cert", status: :committed}}
  end

  test "rejects issuer responses that contain secret material" do
    assert {:error, :acme_issuer_returned_secret_material, receipt} =
             Acme.issue("example.com", [command_id: "cmd-cert-fail"], deps(LeakyIssuer))

    assert receipt.status == :failed
    refute_received {:cert_put, _row}
    assert_received {:command_finish, %{id: "cmd-cert-fail", status: :failed}}
  end

  defp deps(issuer) do
    %{
      commands: Commands,
      leases: Leases,
      certs: Certs,
      transaction: Transaction,
      issuer: issuer
    }
  end
end
