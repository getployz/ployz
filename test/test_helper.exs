ExUnit.start()

defmodule Ployz.TestSupport.Mnesia do
  def reset!(context) do
    dir =
      Path.join(
        System.tmp_dir!(),
        "ployz-mnesia-#{inspect(context.test)}-#{System.unique_integer([:positive])}"
      )

    Ployz.Metadata.Schema.reset_for_test!(dir)

    ExUnit.Callbacks.on_exit(fn ->
      :mnesia.stop()
      File.rm_rf!(dir)
    end)

    :ok
  end
end
