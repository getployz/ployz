defmodule Ployz.Cluster.DistributionTest do
  use ExUnit.Case, async: true

  test "dev mode is explicit" do
    assert :ok = Ployz.Cluster.Distribution.validate_config(:dev, [])
  end

  test "clustered mode requires a private bind address and secure cookie" do
    assert {:error, :missing_bind_address} =
             Ployz.Cluster.Distribution.validate_config(:clustered, cookie_path: "/missing")

    assert {:error, :missing_cookie_path} =
             Ployz.Cluster.Distribution.validate_config(:clustered, bind_address: "10.0.0.1")
  end
end
