# Third-Party Notices

macEverything is distributed under the MIT License (see [LICENSE](LICENSE)).
It bundles or links against third-party components whose licenses are listed here.

## Rust crates

The dependency graph is recorded in [`Cargo.lock`](Cargo.lock). All crates used by
the workspace are licensed under permissive licenses — predominantly
`MIT OR Apache-2.0`, also `Zlib`, `BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unlicense`
and `0BSD`. Run the following to regenerate the full list:

```bash
cargo metadata --format-version 1 | jq -r '.packages[] | "\(.name) \(.version) \(.license)"' | sort -u
```

## Fonts bundled with the app

| Component | License | License text |
|---|---|---|
| Noto Sans SC (Regular) | SIL Open Font License 1.1 | [assets/fonts/OFL.txt](assets/fonts/OFL.txt) |
| Noto Sans KR (Regular) | SIL Open Font License 1.1 | [assets/fonts/OFL.txt](assets/fonts/OFL.txt) |
| Noto Sans Symbols / Symbols 2 | SIL Open Font License 1.1 | [assets/fonts/OFL-NotoSansSymbols.txt](assets/fonts/OFL-NotoSansSymbols.txt) |

These font files are shipped inside `macEverything.app/Contents/Resources/fonts/`
together with their license texts.

## Fonts from `epaint_default_fonts` (egui)

The `epaint_default_fonts` crate is licensed
`(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`. The fonts it embeds are
covered by the following texts, copied into this repository for compliance:

| Font | License | License text |
|---|---|---|
| Ubuntu-Light | Ubuntu Font Licence 1.0 | [docs/licenses/Ubuntu-font-1.0.txt](docs/licenses/Ubuntu-font-1.0.txt) |
| Noto Emoji, egui icons | SIL Open Font License 1.1 | [docs/licenses/epaint-OFL-1.1.txt](docs/licenses/epaint-OFL-1.1.txt) |
| emoji-icon-font | MIT | [docs/licenses/emoji-icon-font-MIT.txt](docs/licenses/emoji-icon-font-MIT.txt) |
| Hack | MIT (plus DejaVu public domain / Bitstream Vera) | [docs/licenses/Hack-Regular.txt](docs/licenses/Hack-Regular.txt) |

## Vendored patch

`vendor/epaint/` contains a patched copy of the `epaint` crate used to keep the
texture atlas delta sizes small. The patch and its rationale are documented in
`vendor/epaint/MCE-PATCH.md`; `epaint` is licensed `MIT OR Apache-2.0`.

## Application icon

`assets/AppIcon.png` / `assets/AppIcon.icns` are original artwork for this project.
