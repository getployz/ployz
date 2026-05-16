defmodule Ployz.MixProject do
  use Mix.Project

  def project do
    [
      app: :ployz,
      version: "0.1.0",
      elixir: "~> 1.18",
      start_permanent: Mix.env() == :prod,
      deps: []
    ]
  end

  def application do
    [
      extra_applications: [:logger, :crypto, :mnesia],
      mod: {Ployz.Application, []}
    ]
  end
end
