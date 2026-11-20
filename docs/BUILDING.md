# Building QCCOM

QCCOM is a plain Cargo project with no code generation or extra build tools. This page covers native builds, cross-compiling Windows binaries from Linux, CI and a few things that are useful when you fork the project.

## Requirements

| | Windows | Linux |
|---|---|---|
| Rust | stable, 1.85 or newer, via [rustup](https://rustup.rs) | same |
| C toolchain | MSVC Build Tools (installed by rustup) | `gcc` / `cc` |
| System packages | none | `pkg-config`, the libudev and libdbus headers |
| Runtime | nothing extra | X11 or Wayland, OpenGL 2+ (Mesa is fine); `xdg-desktop-portal` for the file dialogs |
| Optional | `git` in `PATH` for the Git viewer | same |

Linux packages:

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libudev-dev libdbus-1-dev
# Fedora
sudo dnf install gcc pkgconf-pkg-config systemd-devel dbus-devel
# Arch
sudo pacman -S base-devel pkgconf systemd-libs
```

macOS is not a supported target yet. The code is mostly portable, so a pull request that adds it is welcome.

The portable Windows build and standalone Rust relay use current stable Rust. [Portable build and server instructions](../deploy/remote-com/README.md)

## Native build

```bash
git clone https://github.com/zenghi/QCCOM.git
cd QCCOM
cargo build --release
```

The binary is `target/release/qccom` (`qccom.exe` on Windows). It is self-contained: copy it anywhere and run it. On Windows, `.cargo/config.toml` links the MSVC C runtime statically, so the exe doesn't need the Visual C++ Redistributable.

Run the unit tests with:

```bash
cargo test
```

The tests cover log parsing, the terminal emulator, the C indexer, version comparison, MQTT packet encoding and the translation table. Among other things, they check that every user-visible string in the source has a row in `assets/i18n.tsv`.

## Cross-compiling Windows from Linux

```bash
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
# -> target/x86_64-pc-windows-gnu/release/qccom.exe
```

The GNU build is what the official release uses. It needs no DLLs besides the ones that ship with Windows. `build.rs` embeds the icon (`assets/app.ico`) and the version resource. It uses `winresource`, which finds `x86_64-w64-mingw32-windres` automatically when you cross-compile.

## Release profile

`Cargo.toml` tunes the release profile for size: `opt-level = "z"`, fat LTO, one codegen unit, `strip`, and `panic = "abort"`. A Windows build comes out at about 4 MB and a Linux build at about 7 MB. The difference is TLS: Windows uses the system SChannel through `native-tls`, while Linux bundles `rustls` so it does not depend on the distribution's OpenSSL version.

If you want faster local builds while you work, use the default debug profile (`cargo run`) or temporarily set `lto = false`.

## Running from the command line

```bash
qccom                 # normal start
qccom /dev/ttyUSB0    # preselect a port, which also works for ports the scanner cannot see (e.g. /dev/pts/3)
qccom COM7
```

To test against a virtual serial pair on Linux:

```bash
socat -d -d pty,raw,echo=0 pty,raw,echo=0     # prints two /dev/pts/N paths
qccom /dev/pts/3                               # open one end in QCCOM
cat some.log > /dev/pts/4                      # write to the other end
```

## Where settings are stored

| OS | Path |
|---|---|
| Windows | `%APPDATA%\qccom\data\app.ron` |
| Linux | `~/.local/share/qccom/app.ron` (or `$XDG_DATA_HOME/qccom/`) |

The file is plain RON text and is written automatically. If you delete it, QCCOM starts with the defaults.

## Continuous integration

`.github/workflows/build.yml`:

- On every push and pull request it runs `cargo test` and builds the Linux and Windows binaries on GitHub-hosted runners, then uploads them as workflow artifacts.
- When you push a version tag, the workflow requests a Windows signature from SignPath. After approval and signature validation, it creates a GitHub Release with the signed `qccom-windows-x64.exe`, `qccom-linux-x64`, and `SHA256SUMS.txt` for those exact files.

To publish a release:

```bash
# bump `version` in Cargo.toml first, then
git tag v0.5.1
git push origin v0.5.1
```

Before pushing the tag, complete enrollment with the [SignPath Foundation](https://signpath.org) and configure the SignPath project to sign `qccom-windows-x64.exe` from this repository's Windows build job. Set `SIGNPATH_ORGANIZATION_ID` as a GitHub Actions variable and `SIGNPATH_API_TOKEN` as a GitHub Actions secret. `SIGNPATH_PROJECT_SLUG` and `SIGNPATH_POLICY_SLUG` default to `qccom` and `release-signing`. Approve the request in SignPath after the tagged build submits it. Missing configuration, an invalid signature, or a missing signed artifact stops the release.

For the update server, use the signed EXE from the release. Recompute the SHA-256 and size in `1.json` from that file, then upload the signed EXE before uploading the new manifest. See [the update file format](UPDATE_SERVER.md).

## Notes for forks

- **Update server.** `src/update.rs` points at the author's server (`VERSION_URL`, `DEFAULT_EXE_URL`). Change these constants to your own server, or set `auto_update` off by default, so your builds do not offer the upstream release to your users. When testing, you can override the URL without rebuilding: `QCCOM_UPDATE_URL=http://127.0.0.1:8000/1.json qccom`. The JSON format is described in [UPDATE_SERVER.md](UPDATE_SERVER.md).
- **Attribution.** QCCOM is Apache-2.0 licensed. Keep `LICENSE` and `NOTICE` in your distribution, and keep the attribution line from `NOTICE` ("Based on QCCOM by zenghi – https://github.com/zenghi/QCCOM"). Please leave the author and repository link in the About box.
