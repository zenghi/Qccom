# Contributing to QCCOM

Thanks for helping! Bug reports, ideas, translations and code are all welcome.

## Reporting bugs

Open an [issue](https://github.com/zenghi/QCCOM/issues) and include:

- QCCOM version (shown in the About box) and OS, for example "v0.5.0, Windows 11 23H2"
- the adapter or device, for example "CH340, ESP32-S3 at 115200"
- what you did, what you expected and what happened
- a screenshot or a short log excerpt if possible

If you can reproduce a parsing or display problem with a log file, attach it. That is the fastest way to get it fixed.

## Development setup

See [docs/BUILDING.md](docs/BUILDING.md). In short:

```bash
cargo run            # debug build
cargo test           # all unit tests
cargo build --release
```

Before you open a pull request:

- `cargo test` passes
- `cargo build --release` produces **no warnings**
- `cargo fmt` has been run (`rustfmt.toml` sets `max_width = 200`; CI checks it)
- new user-visible text is translated (see below)
- the change works on both Windows and Linux, or the PR says which one you could not test

Keep pull requests focused: one feature or fix per PR. For larger changes, open an issue first so we can agree on the approach.

## Code style

- Dependencies are kept small on purpose. The release binary is about 4 MB on Windows, so please discuss any new crate in the issue first.
- All I/O (serial, network, git, file indexing, HTTP) runs on background threads and talks to the UI through channels. Never block the UI thread.
- Only visible rows are laid out (`show_rows`). Keep per-frame work proportional to what is on screen.
- Colors, fonts and icons come from `src/theme.rs`. Don't hard-code new colors in the UI code.
- Existing source comments are mostly in Chinese. New comments may be in English or Chinese. Identifiers are English.

## Translations

All UI text lives in one file: [`assets/i18n.tsv`](assets/i18n.tsv). It is tab-separated:

```
# zh-CN	en	zh-TW	ja	ko	de	fr	es	ru	pt
连接	Connect	連線	接続	연결	Verbinden	Connecter	Conectar	Подключить	Conectar
```

- Column 1 is the **key**: the Simplified Chinese text exactly as written in the Rust source, inside `t("…")` or `tf("…", …)`. Don't change keys unless you also change the source.
- The other columns follow the order of `LANGS` in `src/i18n.rs`. An empty cell falls back to English.
- `{}` is a placeholder. Every translation must contain the same number of `{}` as the key, in the order the arguments are passed.
- Escapes are written as in Rust source: `\n` is a line break and `\\` is a backslash.
- Keep translations about as long as the English text. Many labels sit in narrow sidebars and buttons. Check your language in the app at the default window size.

**Fixing a translation:** edit the cell and open a PR. That's all.

**Adding a language:**

1. Append `("xx", "Native name")` to `LANGS` in `src/i18n.rs`, where `xx` is the language code (the primary subtag of the system locale, such as `it`, `pl` or `tr`).
2. Add a column at the end of **every** row in `assets/i18n.tsv`, including the `# zh-CN …` header comment.
3. If the language needs a script that the bundled Inter font doesn't cover (for example Thai, Arabic or Devanagari), add a system-font fallback in `src/theme.rs` next to the existing CJK / Korean / Japanese fallbacks.
4. Run `cargo test`. `table_is_consistent` checks the column count and the placeholders, and `every_string_is_translated` checks that no source string is missing from the table.

**Adding new UI text in code:** wrap it in `t("中文原文")` (or `tf` with `{}` placeholders) and add a row to the TSV with at least the English column filled in. If you don't speak Chinese, write the key in simple Chinese with a translator, or ask in the PR and a maintainer will help.

## License of contributions

QCCOM is licensed under the Apache License 2.0. By submitting a contribution you agree that it is licensed under the same terms (Apache-2.0, section 5), and you confirm that you have the right to submit it.
