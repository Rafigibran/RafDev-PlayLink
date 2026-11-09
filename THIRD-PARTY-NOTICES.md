# Third-Party Notices

RafDev PlayLink uses the following third-party components. Their licenses
apply to those components; the PlayLink application code itself is MIT
(see LICENSE).

## scrcpy server (mirroring backend)

- Project: https://github.com/Genymobile/scrcpy
- License: Apache License 2.0
- Use: the `scrcpy-server` file is downloaded at first run from the scrcpy
  GitHub releases and pushed to the Android device. It is not bundled in
  this repository. The server runs on the device and streams the screen to
  PlayLink over ADB.

## Android platform-tools (ADB)

- Project: https://developer.android.com/tools/adb
- License: Apache License 2.0
- Use: the `adb` executable is used when present on the system, or
  downloaded on first run into the PlayLink app directory when the user
  chooses the bundled download. It is not bundled in this repository.

## OpenH264 (H.264 video decoding)

- Project: https://github.com/cisco/openh264
- License: BSD 2-Clause License
- Use: the `openh264` Rust crate downloads prebuilt Cisco OpenH264 binaries
  at build time and decodes the H.264 video stream from the device.

## Slint (UI framework)

- Project: https://slint.dev
- License: Slint is available under the GNU GPL-3.0, the Slint Royalty-free
  license, or a commercial license. This project uses Slint under the
  royalty-free license terms; see https://slint.dev/pricing for details and
  confirm the license choice that fits your distribution plans.

## Rust crates

All other Rust dependencies are used under their respective licenses
(MIT / Apache-2.0, as declared in their crates.io metadata). Run
`cargo tree` in a checkout to list the exact dependency set.
