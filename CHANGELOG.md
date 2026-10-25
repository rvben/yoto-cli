# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [0.1.1](https://github.com/rvben/yoto-cli/compare/v0.1.0...v0.1.1) - 2026-10-25

### Added

- publish Windows binaries and wheels ([f0f8223](https://github.com/rvben/yoto-cli/commit/f0f8223310dea934a6d4fa6d5a45379c9f1cb534))

### Fixed

- share the keychain lock between processes with other config dirs ([1007a89](https://github.com/rvben/yoto-cli/commit/1007a895356e11ef335509da72a81a10d223f986))
- tell every keychain save apart and never fail one on cleanup ([e3a1880](https://github.com/rvben/yoto-cli/commit/e3a1880d0092f291804fcd56febf1bfddadfd186))
- keep a split sign-in in a fixed set of keychain entries ([76677ae](https://github.com/rvben/yoto-cli/commit/76677ae578c5b73c9bdd5fd7308b488d4dbae37b))
- remove the keychain parts an interrupted save leaves behind ([db33d30](https://github.com/rvben/yoto-cli/commit/db33d30bf9bde63f077414a562ba6e9d34bb0fc6))
- read a split sign-in while another process replaces it ([37d9ef4](https://github.com/rvben/yoto-cli/commit/37d9ef46ba75000539cad1c62e4917d6a8e765c1))
- report a token response cut off mid-body as a network error ([3d91b95](https://github.com/rvben/yoto-cli/commit/3d91b952272205827f4c6ee0e2ed3fdea1a8bbe5))
- store a sign-in longer than one Windows Credential Manager entry ([1a9a836](https://github.com/rvben/yoto-cli/commit/1a9a836148ffed26efe7a013f0e40a8e28123b3d))
- bound the sign-in renewal during a transcode wait by its deadline ([8201b63](https://github.com/rvben/yoto-cli/commit/8201b639c74c2b363679d8f65ec559e2b158ed0e))

## [0.1.0] - 2026-10-25

### Added

- manage Yoto playlists and upload audio with covers and chapter icons ([cf45223](https://github.com/rvben/yoto-cli/commit/cf4522347260c65792e301d8552b942a9bfb67ba))
