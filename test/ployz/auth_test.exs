defmodule Ployz.AuthTest do
  use ExUnit.Case, async: false

  setup do
    previous = Application.get_env(:ployz, :auth_tokens, [])
    on_exit(fn -> Application.put_env(:ployz, :auth_tokens, previous) end)
    :ok
  end

  test "local operator can run privileged commands" do
    assert :ok = Ployz.Auth.authorize(%{role: :local_operator}, :machine_add)
    assert :ok = Ployz.Auth.authorize(%{role: :local_operator}, :machine_remove)
  end

  test "status reader cannot run machine lifecycle commands" do
    assert {:error, :forbidden} = Ployz.Auth.authorize(%{role: :status_reader}, :machine_add)
  end

  test "configured local token is required" do
    Application.put_env(:ployz, :auth_tokens, ["token-a"])

    assert {:error, :missing_token} = Ployz.Auth.authorize(%{role: :local_operator}, :machine_add)

    assert {:error, :invalid_token} =
             Ployz.Auth.authorize(%{role: :local_operator, token: "bad"}, :machine_add)

    assert :ok = Ployz.Auth.authorize(%{role: :local_operator, token: "token-a"}, :machine_add)
  end
end
