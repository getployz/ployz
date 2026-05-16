defmodule Ployz.AuthTest do
  use ExUnit.Case, async: false

  setup do
    previous = Application.get_env(:ployz, :auth_tokens, [])
    previous_mode = Application.get_env(:ployz, :auth_mode, :dev_open)

    on_exit(fn ->
      Application.put_env(:ployz, :auth_tokens, previous)
      Application.put_env(:ployz, :auth_mode, previous_mode)
    end)

    :ok
  end

  test "explicit dev open mode allows local operator without token" do
    Application.put_env(:ployz, :auth_mode, :dev_open)
    assert :ok = Ployz.Auth.authorize(%{role: :local_operator}, :machine_add)
    assert :ok = Ployz.Auth.authorize(%{role: :local_operator}, :machine_remove)
  end

  test "status reader cannot run machine lifecycle commands" do
    assert {:error, :forbidden} = Ployz.Auth.authorize(%{role: :status_reader}, :machine_add)
  end

  test "configured local token is required" do
    Application.put_env(:ployz, :auth_mode, :token)
    Application.put_env(:ployz, :auth_tokens, ["token-a"])

    assert {:error, :missing_token} = Ployz.Auth.authorize(%{role: :local_operator}, :machine_add)

    assert {:error, :invalid_token} =
             Ployz.Auth.authorize(%{role: :local_operator, token: "bad"}, :machine_add)

    assert :ok = Ployz.Auth.authorize(%{role: :local_operator, token: "token-a"}, :machine_add)
  end

  test "non-dev auth mode fails closed without a token" do
    Application.put_env(:ployz, :auth_mode, :token)
    Application.put_env(:ployz, :auth_tokens, [])

    assert {:error, :missing_token} = Ployz.Auth.authorize(%{role: :local_operator}, :machine_add)

    assert {:error, :missing_token_config} =
             Ployz.Auth.authorize(%{role: :local_operator, token: "anything"}, :machine_add)
  end
end
