# Contributing to Phase LAN

Thanks for taking the time to help. Bug reports, ideas, translations and pull requests are all welcome.

## Before you start

- **Bugs and ideas** go to [Issues](https://github.com/phase-mc/phase-lan/issues). Search first, then use the bug or feature template.
- **Security problems** must not go to public issues. See [SECURITY.md](SECURITY.md).
- **Larger changes** (a new feature, a protocol change, a new dependency) should start with an issue, so we can agree on the approach before you write the code.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Setting up

You need Windows 10 or 11, [Rust](https://rustup.rs) stable with the MSVC toolchain and [Node.js](https://nodejs.org) 20.19+ or 22.12+.

```powershell
cd crates\gui
npm ci
npm run tauri dev
```

This starts the window in development mode: Svelte changes show up immediately, and the Rust side rebuilds on its own.

The console app is handy for testing the tunnel without the window:

```powershell
cargo run -p phase-cli -- --help
```

## Checks

Pull requests run the same checks as [CI](.github/workflows/ci.yml). Please run them locally first:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd crates\gui
npm run check
npm test
```

When you only changed one crate, `cargo test -p <crate>` is enough while you work. Some tunnel tests cut the connection on purpose and take 15 to 20 seconds. Tests that need the real internet are marked `#[ignore]`.

## Code style

- Rust: `rustfmt` defaults, no new `clippy` warnings. Comments and doc comments in English.
- Svelte 5 with runes and TypeScript. Screens live in `crates/gui/src/screens`, components in `crates/gui/src/components`, state in `crates/gui/src/lib/*.svelte.ts`.
- **User-facing text is never hard-coded.** In the window, add a key to both `crates/gui/src/lib/i18n/en/*.ts` and `crates/gui/src/lib/i18n/ru/*.ts` and use `t("key")`; the test in `guard.test.ts` fails if text slips past the dictionaries. On the Rust side use `phase_lang::tr!("Russian text", "English text")`.
- Do not add mods to the code. Mod lists come from manifests.
- Keep the window light: animations run on the shared ticker, and there are no infinite CSS animations, `filter` or `backdrop-filter`.
- New dependencies are pinned to an exact version and need a short reason in the pull request.

## Pull requests

- One topic per pull request. Small pull requests are reviewed faster.
- Write commit messages in plain English: a short summary line, then what changed and why if it is not obvious.
- Describe how you tested the change. For anything that touches the connection, say whether you tried it with real Minecraft on two computers.
- Screenshots help for any change in the window, in both the dark and the light theme.

## License

Phase LAN is licensed under MIT OR Apache-2.0. Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
