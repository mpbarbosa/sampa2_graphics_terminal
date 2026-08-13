# Packaging (N3 — Linux citizen)

Build a `.deb` so sampa2 installs like a normal terminal — on `$PATH`, in the app menu,
and registered as an `x-terminal-emulator` alternative.

## Build

```bash
./packaging/build-deb.sh
```

Builds the release binary if needed, then assembles `target/sampa2_<version>_<arch>.deb`
(a single self-contained ~6 MB package — no webview runtime). Needs only `dpkg-deb`; no
`cargo-deb` required. The result is `lintian`-clean.

## Install

```bash
sudo apt install ./target/sampa2_*.deb     # pulls in Recommends (Vulkan driver, etc.)
# or, without apt resolving deps:
sudo dpkg -i ./target/sampa2_*.deb
```

This places:

| Path | |
|---|---|
| `/usr/bin/sampa2` | the binary (stripped) |
| `/usr/share/applications/sampa2.desktop` | app-menu entry (`TerminalEmulator`) |
| `/usr/share/icons/hicolor/*/apps/sampa2.*` | icons (64–512 px + scalable SVG) |
| `/usr/share/man/man1/sampa2.1.gz` | `man sampa2` |

The `postinst` registers `sampa2` as an `x-terminal-emulator` alternative (priority 40).

## What's in the box

- `deb/control` fields (Depends/Recommends) live in [`build-deb.sh`](build-deb.sh). Only
  `libc6`/`libgcc-s1` are hard-linked; the Wayland/X11 + Vulkan stack is dlopened at
  runtime, so `libvulkan1` is a Depend and the driver/window-system libs are Recommends.
- `deb/postinst`, `deb/prerm` — the `x-terminal-emulator` alternative wiring.
- `deb/sampa2.1` — the man page source.
- `deb/copyright` — Debian machine-readable MIT copyright.

## Not yet (rest of N3)

- `--class`/`--title`/`-e`/`--hold`/`--login` CLI (via `sampa-cli`) — the `.desktop`'s
  *edit-config* action already assumes `-e`; it lights up once the CLI lands.
- AppImage and `.rpm` targets.
- `xdg-terminal-exec` integration.
