# Packages the prebuilt release binary (no compilation in %build) — see build-rpm.sh.
# Version/Release are injected with --define at build time.
Name:           sampa2
Version:        %{?_sampa_version}%{!?_sampa_version:0.0.0}
Release:        1%{?dist}
Summary:        Native GPU terminal emulator (winit + wgpu + alacritty_terminal)
License:        MIT
URL:            https://github.com/mpbarbosa/sampa2_graphics_terminal
BuildArch:      x86_64
# The Wayland/X11 + Vulkan stack is dlopened at runtime and varies by distro, so it's a
# soft dependency; the linked libc/libgcc are auto-detected by rpm's find-requires.
Recommends:     mesa-vulkan-drivers
# We ship a stripped, prebuilt binary: no separate debug package, no re-strip.
%global debug_package %{nil}
%global __strip /bin/true

%description
A Rust-only graphical terminal for Linux: an in-app command palette, a live
man-page panel, a safe preview-as-you-type pane, and an opt-in Claude command
suggester. Renders with winit + wgpu + cosmic-text over an alacritty_terminal
VT engine as a single self-contained binary (no webview runtime).

%install
install -Dm0755 %{_sourcedir}/sampa2 %{buildroot}%{_bindir}/sampa2
install -Dm0644 %{_sourcedir}/sampa2.desktop %{buildroot}%{_datadir}/applications/sampa2.desktop
install -Dm0644 %{_sourcedir}/sampa2.1 %{buildroot}%{_mandir}/man1/sampa2.1
gzip -9nf %{buildroot}%{_mandir}/man1/sampa2.1
for sz in 64 128 256 512; do
    install -Dm0644 %{_sourcedir}/sampa2-${sz}.png \
        %{buildroot}%{_datadir}/icons/hicolor/${sz}x${sz}/apps/sampa2.png
done
install -Dm0644 %{_sourcedir}/sampa2-icon.svg \
    %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/sampa2.svg
install -Dm0644 %{_sourcedir}/LICENSE %{buildroot}%{_defaultlicensedir}/%{name}/LICENSE

%files
%license %{_defaultlicensedir}/%{name}/LICENSE
%{_bindir}/sampa2
%{_datadir}/applications/sampa2.desktop
%{_mandir}/man1/sampa2.1.gz
%{_datadir}/icons/hicolor/*/apps/sampa2.png
%{_datadir}/icons/hicolor/scalable/apps/sampa2.svg

%post
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q %{_datadir}/applications || :
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t -f %{_datadir}/icons/hicolor || :

%postun
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q %{_datadir}/applications || :

%changelog
* Thu Jan 01 1970 Marcelo Pereira Barbosa <mpbarbosa@gmail.com> - 0.1.0-1
- Native (Path C) build package.
