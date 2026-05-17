# Changelog

## [1.1.0](https://github.com/InstaZDLL/WaveFlow/compare/v1.0.0...v1.1.0) (2026-05-17)


### Features

* **apt:** publish .deb to Buildkite Packages registry on release ([328367f](https://github.com/InstaZDLL/WaveFlow/commit/328367f8fd2ae96dba34742c6efdcafd61cd47b9))
* **apt:** publish .deb to Buildkite Packages registry on release ([cb9a4bc](https://github.com/InstaZDLL/WaveFlow/commit/cb9a4bccccb9373e00ab2927053a259ea882fff0))
* **artist:** add UI to pick or remove the artist image ([d277519](https://github.com/InstaZDLL/WaveFlow/commit/d277519aef7ed0b999b1888f748c44b191384de4))
* **backup:** optionally bundle shared Deezer artwork cache ([03365ca](https://github.com/InstaZDLL/WaveFlow/commit/03365ca3fc01e3b8aae9373cd33eb98efc93c88b))
* **backup:** optionally bundle shared Deezer artwork cache ([932106e](https://github.com/InstaZDLL/WaveFlow/commit/932106e2cac4128170a7377a7e05c09bc9ff36fc))
* **config:** add .coderabbit.yaml for project configuration and review instructions ([cc72976](https://github.com/InstaZDLL/WaveFlow/commit/cc729762a4e4ebe3d31daac1f274fc42d77c4762))
* **distribution:** publish to fedora copr on every release ([8af4104](https://github.com/InstaZDLL/WaveFlow/commit/8af41043fde2ee502503dd8acf84ea3753495bee))
* **distribution:** publish to winget-pkgs on every release ([e069679](https://github.com/InstaZDLL/WaveFlow/commit/e0696791076e6ad20d479cc0e4d067dc591807be))
* **library:** enhance create playlist functionality with source tracking ([3b806bf](https://github.com/InstaZDLL/WaveFlow/commit/3b806bfefd782622d0ce515c640052296bcf5245))
* **library:** local artist images + picker UI ([#33](https://github.com/InstaZDLL/WaveFlow/issues/33)) ([af719b6](https://github.com/InstaZDLL/WaveFlow/commit/af719b6841423b5080b88494dd9c2cc5f9ab0b03))
* **library:** open genre detail page from tags grid ([#23](https://github.com/InstaZDLL/WaveFlow/issues/23)) ([0a5fbb9](https://github.com/InstaZDLL/WaveFlow/commit/0a5fbb9108be3ae6035f8751c846e671f8500628))
* **library:** use local artist.jpg sidecar as artist photo ([e23ece4](https://github.com/InstaZDLL/WaveFlow/commit/e23ece4b5465cd962afb0b0537f0311c67735082))
* **lyrics:** word-level karaoke (enhanced LRC + TTML) ([7aeb19e](https://github.com/InstaZDLL/WaveFlow/commit/7aeb19ee9d953e99591ceb4faca5d27d67d9539d))
* **lyrics:** word-level karaoke (enhanced LRC + TTML) ([#25](https://github.com/InstaZDLL/WaveFlow/issues/25)) ([62701c3](https://github.com/InstaZDLL/WaveFlow/commit/62701c3c8fa6c85d968ae609efda253074c50bad))
* **player-bar:** host A-B loop + sleep timer in overflow menu by default ([1a7aabb](https://github.com/InstaZDLL/WaveFlow/commit/1a7aabbf5dc552f1d5fec4ac34bd862e5b767f7e))
* **player-bar:** move playback speed into the overflow menu ([c322e54](https://github.com/InstaZDLL/WaveFlow/commit/c322e54269f9682c355b3f177c2ee000a198556c))
* **player-bar:** redesign right cluster (Spotify-style) + overflow defaults ([ade9f13](https://github.com/InstaZDLL/WaveFlow/commit/ade9f1379faefe3f1be61d6ccd60b70902131258))
* **player-bar:** spotify-style mini-player + fullscreen icons after volume ([7d58651](https://github.com/InstaZDLL/WaveFlow/commit/7d58651ddcec4259014deb6465140dc5431eef34))
* **playlist:** add filename sort mode ([83b5fda](https://github.com/InstaZDLL/WaveFlow/commit/83b5fda3abd054cbd337415c3276c144331866c7))
* **playlist:** add filename sort mode ([525ff85](https://github.com/InstaZDLL/WaveFlow/commit/525ff85a21413eb14a8110fd6f7cc56ed32be384))
* **playlist:** add Spotify-style sort modes with per-playlist memory ([685bfbf](https://github.com/InstaZDLL/WaveFlow/commit/685bfbff6082b55daa30bcb8587e78217a6d75b7))
* **scan:** auto-merge implicit compilations into Various Artists ([1b24e7e](https://github.com/InstaZDLL/WaveFlow/commit/1b24e7e9b1cb4e6a7d5c908d3b270672c55c5227))
* **ui:** add splash screen to mask cold-start delay ([#26](https://github.com/InstaZDLL/WaveFlow/issues/26)) ([813a38d](https://github.com/InstaZDLL/WaveFlow/commit/813a38d64ca6240e6d74bcd1d3acacf3292473a9))
* **ui:** drop the beta badge from the sidebar brand ([e731834](https://github.com/InstaZDLL/WaveFlow/commit/e73183477f0ba778fbddb069dcf0c8ad5e2968d4))
* **ui:** make album cell clickable in playlist + library track rows ([209b8be](https://github.com/InstaZDLL/WaveFlow/commit/209b8bebda8ad56f5346bed93e77422eb6e0e6fa))


### Bug Fixes

* **about:** read version from Tauri at runtime + bump to 1.0.1 ([829ec89](https://github.com/InstaZDLL/WaveFlow/commit/829ec89f8f7196d9d69b28588eba42163b96e98c))
* **about:** read version from Tauri at runtime instead of hardcoded 0.1.0 ([6bdc69d](https://github.com/InstaZDLL/WaveFlow/commit/6bdc69debbcd2b5fd25d88304de98536dc28fa3c))
* **apt:** address coderabbit review ([285a5f1](https://github.com/InstaZDLL/WaveFlow/commit/285a5f104b5038b368bd70633dcf7a1fd5a86db5))
* **audio:** handle panics in decoder thread and improve WASAPI exclusive mode management ([27cfdf7](https://github.com/InstaZDLL/WaveFlow/commit/27cfdf7a262ea7627abf828e0a14219ff8e83c62))
* **audio:** keep splash close from stopping playback ([a4fdaa9](https://github.com/InstaZDLL/WaveFlow/commit/a4fdaa90cf6645d7e668a8ddc560d930f3e504a2))
* **backup:** preserve metadata-artwork flag on archive failure + fr typo ([9b8030d](https://github.com/InstaZDLL/WaveFlow/commit/9b8030d931d19e0569fb41a2ffa6488392cd0081))
* **ci:** copr project name is lowercase 'instazdll/waveflow' ([6b4235d](https://github.com/InstaZDLL/WaveFlow/commit/6b4235d0288de42e169c8a008ffdcce07e6d99c0))
* **ci:** copr workflow checks out main instead of the release tag ([37e7dd1](https://github.com/InstaZDLL/WaveFlow/commit/37e7dd1397e9e5becc8d07586c9dcf2ae24114a6))
* **ci:** plug script injection in lockfile-build (head.ref attacker-controlled) ([ac13cae](https://github.com/InstaZDLL/WaveFlow/commit/ac13cae1f33f3b532955a5076cc75aa6c1c8f544))
* **ci:** split release-please lockfile pipeline (codeql untrusted-checkout/critical) ([9db6031](https://github.com/InstaZDLL/WaveFlow/commit/9db60314e81700bfd7c302e3cc2efd9a63b924e9))
* **ci:** split release-please lockfile pipeline (codeql untrusted-checkout/critical) ([49645b0](https://github.com/InstaZDLL/WaveFlow/commit/49645b056069786178da6e21778ef8413dc5ae4d))
* **copr:** match the upstream rpm's mixed-case file layout ([093a516](https://github.com/InstaZDLL/WaveFlow/commit/093a51651009f9f2671c3d6ecd8c854e13b650d9))
* **decoder:** simplify parameter passing in decoder_loop function ([ebd4d85](https://github.com/InstaZDLL/WaveFlow/commit/ebd4d851d501e3f9f21a8b5efde30749e9e9a626))
* **deezer:** surface result of missing-cover batch fetch ([4bb905b](https://github.com/InstaZDLL/WaveFlow/commit/4bb905ba7299fe4c8b476f3751c83746df5250ec))
* **feedback:** correct contact email domain + wire mailto handler ([6cf797f](https://github.com/InstaZDLL/WaveFlow/commit/6cf797ff397d8369811ecc46f4877791eb18422c))
* **home:** avoid nested button in recently played tile ([fb0eff4](https://github.com/InstaZDLL/WaveFlow/commit/fb0eff41b7dc6aac64858dbf615511518c73b8fe))
* **locale:** add missing spotify.emptyPlaylist key to 15 locales ([fe3aa19](https://github.com/InstaZDLL/WaveFlow/commit/fe3aa197e8422724a7fca5b54fb7b382c68668a5))
* **locale:** declare missing keys in playlist modal, spotify integration, and progress bar ([dd1d7a7](https://github.com/InstaZDLL/WaveFlow/commit/dd1d7a7bffab98600505bdfda218c53a8142f6ea))
* **locale:** translate home daily mix section and tray menu ([3475280](https://github.com/InstaZDLL/WaveFlow/commit/34752801783815700422649b6a06930f55288716))
* **locale:** translate home daily mix section and tray menu ([#32](https://github.com/InstaZDLL/WaveFlow/issues/32)) ([9bbdd64](https://github.com/InstaZDLL/WaveFlow/commit/9bbdd64b3facb6cbc310442a817fda2790c7b0b5))
* **lyrics:** address coderabbit review ([ddd89ff](https://github.com/InstaZDLL/WaveFlow/commit/ddd89ff4ed3dbbd6438a17cee9f6dda83950563b))
* **lyrics:** bind prefix word to each duplicated line stamp ([ecd87c1](https://github.com/InstaZDLL/WaveFlow/commit/ecd87c127f753d765db822383e4269739d438cf8))
* **lyrics:** build enhanced LRC word stamps directly (codeql) ([3b80c8e](https://github.com/InstaZDLL/WaveFlow/commit/3b80c8e5e4fc3ba3f55b0b308f709d9e04d3e22c))
* **lyrics:** keep unstamped text rows on word-mode save ([4240f9d](https://github.com/InstaZDLL/WaveFlow/commit/4240f9db8a2c4a9782306922f4a59f3f3eff77a4))
* **lyrics:** keep word/ttml badge intact in the panel footer ([fb9c7bf](https://github.com/InstaZDLL/WaveFlow/commit/fb9c7bfa1b8419bf866649379021f79339292582))
* **lyrics:** mirror fullscreen word animation in the side panel ([da44f8b](https://github.com/InstaZDLL/WaveFlow/commit/da44f8b3b05074d7201a12e460b1a2371a62dd71))
* **migration:** force file_modified=0 so album_artist backfill triggers ([277871d](https://github.com/InstaZDLL/WaveFlow/commit/277871d35fbcac40beebcca0a27ee80485bc1297))
* **nav:** preserve detail payloads in history + don't toggle off shuffle ([#24](https://github.com/InstaZDLL/WaveFlow/issues/24)) ([981144c](https://github.com/InstaZDLL/WaveFlow/commit/981144c50a972b42f70afa3875fd3d5fcf7f1bee))
* **packaging:** drop duplicate ReleaseNotesUrl from winget installer manifest ([f62b242](https://github.com/InstaZDLL/WaveFlow/commit/f62b2429ffd6d008376ad0f533233c2aea8fc33e))
* **player-bar:** address CodeRabbit review on MoreActionsMenu ([6458185](https://github.com/InstaZDLL/WaveFlow/commit/6458185a293f7148b473a27057559663a8a74512))
* **playlist-cover:** dedupe artwork hashes before composing auto-cover ([b75e80c](https://github.com/InstaZDLL/WaveFlow/commit/b75e80c03d3f9054131b5feffb25569547f8c0e3))
* **playlist:** drop dead i18n fallback for sort.filename ([a2e0467](https://github.com/InstaZDLL/WaveFlow/commit/a2e0467dad28bfa911d46d63a8a7cd505acd72d0))
* **profile:** normalise sqlx migration checksums on import + pin sources to LF ([#27](https://github.com/InstaZDLL/WaveFlow/issues/27)) ([521e484](https://github.com/InstaZDLL/WaveFlow/commit/521e4842d4de7cd9537e50d15aa12cc3d85149b7))
* **profile:** roll back partial import on checksum/migrate failure ([#28](https://github.com/InstaZDLL/WaveFlow/issues/28)) ([921678b](https://github.com/InstaZDLL/WaveFlow/commit/921678be8493c56d78248ab8a2f8487a67f7afe4))
* **review:** address PR [#33](https://github.com/InstaZDLL/WaveFlow/issues/33) review feedback ([f67f496](https://github.com/InstaZDLL/WaveFlow/commit/f67f496bb8018c5af4b935937e747be2a8a0d6f9))
* **review:** batch rescan tx + modal i18n and request-id cleanup ([a3429bc](https://github.com/InstaZDLL/WaveFlow/commit/a3429bce43e2edb1a8b406044570eb696068b82d))
* **scan:** group albums by Album Artist tag + compilation flag ([d341e5f](https://github.com/InstaZDLL/WaveFlow/commit/d341e5f1ed9be6125336e6163f5b96daf0534ebe))
* **settings:** persist autostart, close-to-tray and scan-on-start toggles ([35e3719](https://github.com/InstaZDLL/WaveFlow/commit/35e3719cf767d00943ae47f86f914896f3fe7491))
* **theme:** persist preference before view transition ([415dbbe](https://github.com/InstaZDLL/WaveFlow/commit/415dbbe23ff88aa56eac9e083218636fa6971205))
* **theme:** persist preference before view transition ([aaeeec7](https://github.com/InstaZDLL/WaveFlow/commit/aaeeec75ae297873244f9e3e623b5485a53a231c)), closes [#34](https://github.com/InstaZDLL/WaveFlow/issues/34)
* **ui:** cap grid item width on wide screens (auto-fill instead of fixed cols) ([36c44a1](https://github.com/InstaZDLL/WaveFlow/commit/36c44a19ab5e26258950e9ba6ae76feeb7c33b20))
* **ui:** cap grid item width on wide screens (auto-fill instead of fixed cols) ([#22](https://github.com/InstaZDLL/WaveFlow/issues/22)) ([194d7ee](https://github.com/InstaZDLL/WaveFlow/commit/194d7ee8311cd53fcfc3c91dc59569e376b041ce))
* **workflow:** enhance security by restricting PR execution to github-actions[bot] ([6b993df](https://github.com/InstaZDLL/WaveFlow/commit/6b993dfe5fd81b7d836703e04de275947f932e12))


### Performance Improvements

* **scan:** skip album backfill UPDATE when scan has no new tag info ([1b44737](https://github.com/InstaZDLL/WaveFlow/commit/1b447375d32955868ef1608812cca23be1661e38))

## 1.0.0 (2026-05-12)

Initial stable release. See the [GitHub Release](https://github.com/InstaZDLL/WaveFlow/releases/tag/v1.0.0) for the full set of features shipped in 1.0.
