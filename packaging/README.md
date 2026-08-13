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

## AppImage (single file, no install)

```bash
./packaging/build-appimage.sh          # → target/sampa2-<version>-x86_64.AppImage
./target/sampa2-*-x86_64.AppImage      # runs anywhere; --version / --help / any flag
```

Auto-fetches `appimagetool` (cached in `target/`) if it isn't on `$PATH`. The binary
hard-links only `libc6`/`libgcc-s1`; the Wayland/X11 + Vulkan stack is dlopened from the
host, so libs are **deliberately not bundled** — the image stays ~8 MB and uses the host's
display/GPU drivers (same expectation as the `.deb`).

## RPM (Fedora / RHEL / openSUSE)

```bash
./packaging/build-rpm.sh               # → target/sampa2-<version>-1.<dist>.x86_64.rpm
sudo dnf install ./target/sampa2-*.rpm
```

Packages the prebuilt binary via `rpmbuild` (`packaging/rpm/sampa2.spec`). On an RPM distro
`rpmbuild` is stock; on Debian/Ubuntu install it with `sudo apt install rpm`. rpm's
find-requires auto-detects the `libc`/`libgcc` deps; `mesa-vulkan-drivers` is a Recommends
(the display/Vulkan stack is dlopened, like the other formats).

## What's in the box

- `deb/control` fields (Depends/Recommends) live in [`build-deb.sh`](build-deb.sh). Only
  `libc6`/`libgcc-s1` are hard-linked; the Wayland/X11 + Vulkan stack is dlopened at
  runtime, so `libvulkan1` is a Depend and the driver/window-system libs are Recommends.
- `deb/postinst`, `deb/prerm` — the `x-terminal-emulator` alternative wiring.
- `deb/sampa2.1` — the man page source.
- `deb/copyright` — Debian machine-readable MIT copyright.

## CLI

The launcher flags are wired (via the shared `sampa-cli` parser), so the `.desktop`'s
*edit-config* action and `x-terminal-emulator` `-e` usage work:

```
sampa2 -e CMD [ARGS…]      # or `-- CMD …` — run CMD instead of $SHELL
       -w, --working-directory DIR
       -T, --title STR
           --class STR      # WM_CLASS (X11 + Wayland app id)
           --config FILE
           --hold           # keep the window open after the command exits
       -l, --login          # start $SHELL as a login shell
       -h/--help, -V/--version
```

## Portability (glibc floor)

The binary needs the glibc it was **built** against — building on Ubuntu 26.04 produces a
binary that requires `GLIBC_2.43`, so it won't run on Ubuntu 24.04 (2.39) / Debian 13
(2.41). `build-deb.sh` pins `Depends: libc6 (>= <detected>)` from the binary's max
`GLIBC_*` symbol, so **apt refuses to install on too-old systems** rather than installing a
binary that dies with `GLIBC_x.y not found` (verified in a clean 24.04 container). The
`.rpm` gets this automatically (rpm's find-requires emits versioned `libc.so.6(GLIBC_x.y)`).
The **AppImage can't declare deps**, so it simply requires that glibc at runtime.

**For wide reach, build on an older base** (e.g. Ubuntu 22.04 in CI) to get a lower floor.

## Default-terminal integration

- **Debian/Ubuntu:** the `.deb` `postinst` registers an `x-terminal-emulator` alternative.
- **freedesktop `xdg-terminal-exec`:** the `.desktop` carries `X-TerminalArg{Exec,Title,
  AppId,Dir,Hold}` keys mapping each capability to the matching sampa-cli flag, so
  `xdg-terminal-exec` drives sampa2 correctly (e.g. `--title=X -- htop` →
  `sampa2 --title X -e htop`). Make it your preferred terminal with:

  ```bash
  echo sampa2.desktop > ~/.config/xdg-terminals.list
  ```

That's the full N3 packaging + terminal-integration set (`.deb` · AppImage · `.rpm` · CLI ·
`x-terminal-emulator` · `xdg-terminal-exec`).
