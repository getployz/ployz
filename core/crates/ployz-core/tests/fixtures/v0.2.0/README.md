Frozen. These files are what a Ployz 0.2.0 daemon stored and spoke. Never edit
or delete them; a later daemon must keep reading them. A new release adds its
own `fixtures/vX.Y.Z/` directory. Checked by `tests/frozen_formats.rs` here and
`src/frozen_format_tests.rs` in ployzd.
