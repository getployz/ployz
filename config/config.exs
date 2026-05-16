import Config

config :ployz,
  auth_mode: :dev_open,
  distribution_mode: :dev,
  metadata_storage: :ram,
  metadata_dir: System.get_env("PLOYZ_MNESIA_DIR") || "tmp/mnesia/#{Mix.env()}",
  auth_tokens: []
