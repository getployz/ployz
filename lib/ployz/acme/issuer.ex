defmodule Ployz.Acme.Issuer do
  @moduledoc """
  Placeholder ACME issuer boundary for the v2 MVP.

  Real ACME integration belongs behind the helper-owned secret/certificate
  boundary. The default module returns a structured foreground failure instead
  of crashing command dispatch when no issuer has been configured.
  """

  @spec issue(String.t(), timeout()) :: {:ok, map()} | {:error, map()}
  def issue(hostname, _timeout) when is_binary(hostname) do
    {:error,
     %{
       code: :acme_issuer_not_configured,
       hostname: hostname,
       detail: "configure an ACME issuer helper before running cert issue"
     }}
  end
end
