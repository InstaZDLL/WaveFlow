%global upstream_url https://github.com/InstaZDLL/WaveFlow
%global asset_url    %{upstream_url}/releases/download/v%{version}/WaveFlow_%{version}_linux-x86_64.rpm

# The upstream Tauri binary is already stripped, so there is no
# debug info for Fedora's automatic `find-debuginfo` step to extract.
# Disable the debug subpackage so rpmbuild doesn't try (and emit a
# bunch of warnings) — it has nothing to operate on.
%global debug_package %{nil}

Name:           waveflow
Version:        1.0.0
Release:        1%{?dist}
Summary:        Local-first music player with Hi-Res audio and word-synced lyrics

License:        GPL-3.0-only
URL:            https://waveflow.app

ExclusiveArch:  x86_64

Source0:        %{asset_url}

# The upstream .rpm was built by tauri-bundler on Ubuntu and its
# auto-detected Requires use Debian-style sonames that confuse
# dnf on Fedora. Override with native Fedora package names below.
AutoReqProv:    no

Requires:       webkit2gtk4.1
Requires:       gtk3
Requires:       libsoup3
Requires:       libayatana-appindicator-gtk3
Requires:       librsvg2
Requires:       alsa-lib

%description
WaveFlow is a local-first music player for Windows, macOS and Linux. It
scans your music folders, organizes everything by album, artist and
genre, and plays it through its own real-time audio engine. No account,
no subscription, no streaming service: your music stays on your machine.

Hi-Res audio, bit-perfect when you ask for it, with exclusive output,
native DSD, 5.1 and 7.1, gapless playback and AutoMix crossfades that
land on the beat. Lyrics are synced word by word, with romanization and
translations, in the app, the mini-player or a floating desktop window.
Smart playlists, Daily Mix and Release Radar build sessions from your
library, MusicBrainz fills its gaps, and a tag workshop edits it with an
undo.

Six skins and fourteen themes, podcasts, WebDAV and SMB shares, Last.fm
and ListenBrainz scrobbling, DLNA and MPD servers, your own WaveFlow
Server, and sandboxed plugins for more lyrics sources, animated covers
and web radio.

%prep
# Source0 is a binary RPM, not a tarball — nothing to extract here.
# %setup -T -c creates an empty build directory so rpmbuild is happy.
%setup -T -c

%build
# Repackaging only — no compile step.

%install
# Drop the upstream .rpm payload directly into the build root.
mkdir -p %{buildroot}
cd %{buildroot}
rpm2cpio %{SOURCE0} | cpio -idmv

%files
# Tauri-bundler emits a mixed-case layout: the binary and icon
# basenames are lowercase, but the .desktop file keeps the
# upstream productName casing.
%{_bindir}/waveflow
%{_datadir}/applications/WaveFlow.desktop
%{_datadir}/icons/hicolor/*/apps/waveflow.png
# Bundled plugins layout — `/usr/lib/WaveFlow/plugins/<name>/`
# carries each plugin's manifest.toml + plugin.wasm, planted by
# tauri-bundler from `src-tauri/resources/plugins/<name>/` during the .rpm
# build. Listing the whole tree as a single entry keeps the spec
# stable as more plugins are bundled in future releases without
# touching this file — anything that lands under that prefix
# gets owned by the package automatically.
%{_prefix}/lib/WaveFlow

%changelog
* Wed May 13 2026 InstaZDLL <github.105mh@8shield.net> - 1.0.0-1
- Initial COPR release. Repackages the upstream .rpm from GitHub
  Releases with Fedora-native Requires.
