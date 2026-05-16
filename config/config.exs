import Config

config :ployz,
  metadata_storage: :disc,
  metadata_dir: System.get_env("PLOYZ_MNESIA_DIR") || "tmp/mnesia/#{Mix.env()}",
  auth_tokens: []
