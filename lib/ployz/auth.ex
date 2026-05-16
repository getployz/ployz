defmodule Ployz.Auth do
  @moduledoc false

  @permissions %{
    machine_add: [:local_operator],
    machine_remove: [:local_operator],
    deploy: [:local_operator],
    cert_issue: [:local_operator],
    migrate_volume: [:local_operator],
    gateway_routes: [:local_operator, :status_reader],
    status: [:local_operator, :status_reader]
  }

  def authorize(%{role: role} = actor, command) do
    with :ok <- authorize_token(actor),
         :ok <- authorize_role(role, command) do
      :ok
    end
  end

  def authorize(_actor, _command), do: {:error, :unauthorized}

  defp authorize_token(%{token: token}) when is_binary(token) do
    tokens = Application.get_env(:ployz, :auth_tokens, [])

    cond do
      tokens == [] -> :ok
      token in tokens -> :ok
      true -> {:error, :invalid_token}
    end
  end

  defp authorize_token(_actor) do
    case Application.get_env(:ployz, :auth_tokens, []) do
      [] -> :ok
      _tokens -> {:error, :missing_token}
    end
  end

  defp authorize_role(role, command) do
    if role in Map.get(@permissions, command, []) do
      :ok
    else
      {:error, :forbidden}
    end
  end
end
