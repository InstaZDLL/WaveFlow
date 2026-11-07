#!/usr/bin/env python3
"""Collect one platform's release assets and write its updater manifest part.

    prepare-assets.py <linux|windows|macos> --base-url <url>

Run from the repository root after `tauri build`. Copies the bundles
under their release names into `dist-release/` and writes
`dist-manifest/latest-<platform>.json`, whose payload URLs are
`<base-url>/<asset name>`.

The base URL is the one thing that differs between hosts, so it is an
argument and never derived here: `release.yml` passes the GitHub release
(`https://github.com/<repo>/releases/download/<tag>`), `.gitlab-ci.yml`
the R2 folder of the tag. Everything else — names, which file is the
updater payload, which signature goes with it — is one contract, kept in
one place for both pipelines.

Reads `VERSION` and `IS_PRERELEASE` from the environment, as the
workflows set them.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import shutil
import sys
from datetime import datetime, timezone


def fail(message: str) -> None:
    print(f"::error::{message}", file=sys.stderr)
    sys.exit(1)


def pub_date() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.000Z")


def write_manifest(dist_manifest: pathlib.Path, platform: str, manifest: dict) -> None:
    (dist_manifest / f"latest-{platform}.json").write_text(
        json.dumps(manifest, indent=2) + "\n",
        encoding="utf-8",
    )


def linux(version: str, base_url: str, dist_release: pathlib.Path, dist_manifest: pathlib.Path) -> None:
    root = pathlib.Path("src-tauri/target/release/bundle")

    # AppImage is the updater's payload format on Linux. The
    # .deb and .rpm are end-user installers for native package
    # managers (apt/dnf) — they don't carry an updater payload.
    appimage_dir = root / "appimage"
    if not appimage_dir.exists():
        fail("No AppImage bundle directory found")

    appimages = sorted(appimage_dir.glob("*.AppImage"))
    if not appimages:
        fail("No .AppImage produced under bundle/appimage/")
    appimage = appimages[0]
    renamed = f"WaveFlow_{version}_linux-x86_64.AppImage"
    shutil.copy2(appimage, dist_release / renamed)

    # Tauri 2 signs the AppImage directly — no .tar.gz wrapper.
    # The AppImage *is* the updater payload; the .sig sits next
    # to it. (Older Tauri 1.x produced an extra .AppImage.tar.gz;
    # we keep a fallback for forward-compat if Tauri ever brings
    # it back.)
    updater_sig = appimage_dir / f"{appimage.name}.sig"
    legacy_payload = appimage_dir / f"{appimage.name}.tar.gz"
    legacy_sig = appimage_dir / f"{appimage.name}.tar.gz.sig"

    if updater_sig.exists():
        sig_renamed = f"{renamed}.sig"
        shutil.copy2(updater_sig, dist_release / sig_renamed)
        payload_filename = renamed  # the AppImage itself
        sig_path_for_manifest = updater_sig
    elif legacy_payload.exists() and legacy_sig.exists():
        payload_renamed = f"{renamed}.tar.gz"
        sig_renamed = f"{renamed}.tar.gz.sig"
        shutil.copy2(legacy_payload, dist_release / payload_renamed)
        shutil.copy2(legacy_sig, dist_release / sig_renamed)
        payload_filename = payload_renamed
        sig_path_for_manifest = legacy_sig
    else:
        fail("Missing AppImage updater signature (looked for .AppImage.sig and .AppImage.tar.gz.sig)")

    # rpm refuses a "-" in its version, so a pre-release has no .rpm and
    # the build does not ask for one; a stable release must have both.
    prerelease = "-" in version
    installers = {}
    for folder, pattern, suffix in [
        ("deb", "*.deb", "deb"),
        ("rpm", "*.rpm", "rpm"),
    ]:
        folder_path = root / folder
        matches = sorted(folder_path.glob(pattern)) if folder_path.exists() else []
        if not matches:
            if suffix == "rpm" and prerelease:
                print(f"No .rpm for pre-release {version} (rpm rejects '-' in a version).")
                continue
            fail(f"No {pattern} produced under bundle/{folder}/")
        installers[suffix] = f"WaveFlow_{version}_linux-x86_64.{suffix}"
        shutil.copy2(matches[0], dist_release / installers[suffix])

    signature = sig_path_for_manifest.read_text(encoding="utf-8").strip()
    write_manifest(dist_manifest, "linux", {
        "version": version,
        "notes": "",
        "pub_date": pub_date(),
        "platforms": {
            "linux-x86_64": {
                "signature": signature,
                "url": f"{base_url}/{payload_filename}",
            }
        },
    })
    print(json.dumps({
        "appimage": renamed,
        **installers,
        "updater_payload": payload_filename,
        "updater_sig": sig_renamed,
    }, indent=2))


def windows(version: str, base_url: str, dist_release: pathlib.Path, dist_manifest: pathlib.Path) -> None:
    root = pathlib.Path("src-tauri/target/release/bundle")

    # NSIS is the updater's payload format (per-user install
    # under %LOCALAPPDATA%). The MSI is the system-wide
    # installer (Program Files) for IT/corporate deployment —
    # it's signed by the same Authenticode pass but doesn't
    # carry an updater signature.
    nsis_dir = root / "nsis"
    if not nsis_dir.exists():
        fail("No NSIS bundle directory found")

    installers = sorted(nsis_dir.glob("*-setup.exe"))
    if not installers:
        fail("No NSIS *-setup.exe produced")
    installer = installers[0]
    renamed = f"WaveFlow_{version}_windows-x86_64-setup.exe"
    shutil.copy2(installer, dist_release / renamed)

    updater_sig = installer.with_suffix(installer.suffix + ".sig")
    if not updater_sig.exists():
        fail("Missing .sig next to NSIS installer")
    sig_renamed = f"{renamed}.sig"
    shutil.copy2(updater_sig, dist_release / sig_renamed)

    # Beta builds ship NSIS only — WiX/MSI can't take a non-numeric
    # pre-release version, so the MSI is expected to be absent there.
    is_prerelease = os.environ.get("IS_PRERELEASE") == "true"
    msi_dir = root / "msi"
    msi_files = sorted(msi_dir.glob("*.msi")) if msi_dir.exists() else []
    msi_renamed = None
    if msi_files:
        msi_renamed = f"WaveFlow_{version}_windows-x86_64.msi"
        shutil.copy2(msi_files[0], dist_release / msi_renamed)
    elif is_prerelease:
        print("No MSI on the beta channel (NSIS-only Windows build) — skipping.")
    else:
        fail("No MSI produced under bundle/msi/")

    signature = updater_sig.read_text(encoding="utf-8").strip()
    write_manifest(dist_manifest, "windows", {
        "version": version,
        "notes": "",
        "pub_date": pub_date(),
        "platforms": {
            "windows-x86_64": {
                "signature": signature,
                "url": f"{base_url}/{renamed}",
            }
        },
    })
    print(json.dumps({
        "nsis_setup": renamed,
        "msi": msi_renamed,
        "updater_sig": sig_renamed,
    }, indent=2))


def macos(version: str, base_url: str, dist_release: pathlib.Path, dist_manifest: pathlib.Path) -> None:
    # When --target universal-apple-darwin is passed, bundles
    # land under target/<triple>/release/bundle/ instead of the
    # default target/release/bundle/.
    target_dir = pathlib.Path("src-tauri/target/universal-apple-darwin/release/bundle")
    if not target_dir.exists():
        target_dir = pathlib.Path("src-tauri/target/release/bundle")

    # The DMG is what humans download for installation.
    dmg_dir = target_dir / "dmg"
    if not dmg_dir.exists():
        fail("No DMG bundle directory found")
    dmgs = sorted(dmg_dir.glob("*.dmg"))
    if not dmgs:
        fail("No .dmg produced under bundle/dmg/")
    dmg = dmgs[0]
    renamed_dmg = f"WaveFlow_{version}_macos-universal.dmg"
    shutil.copy2(dmg, dist_release / renamed_dmg)

    # The .app.tar.gz + .sig is what the updater consumes.
    macos_dir = target_dir / "macos"
    if not macos_dir.exists():
        fail("No macos bundle directory found")
    payloads = sorted(macos_dir.glob("*.app.tar.gz"))
    sigs = sorted(macos_dir.glob("*.app.tar.gz.sig"))
    if not payloads or not sigs:
        fail("Missing updater payload/signature for macOS")
    payload_renamed = f"WaveFlow_{version}_macos-universal.app.tar.gz"
    sig_renamed = f"{payload_renamed}.sig"
    shutil.copy2(payloads[0], dist_release / payload_renamed)
    shutil.copy2(sigs[0], dist_release / sig_renamed)

    signature = sigs[0].read_text(encoding="utf-8").strip()
    download_url = f"{base_url}/{payload_renamed}"
    # Tauri 2's updater accepts a single darwin-universal entry
    # for universal binaries.
    write_manifest(dist_manifest, "macos", {
        "version": version,
        "notes": "",
        "pub_date": pub_date(),
        "platforms": {
            "darwin-x86_64": {
                "signature": signature,
                "url": download_url,
            },
            "darwin-aarch64": {
                "signature": signature,
                "url": download_url,
            },
        },
    })
    print(json.dumps({"dmg": renamed_dmg, "payload": payload_renamed, "sig": sig_renamed}, indent=2))


PLATFORMS = {"linux": linux, "windows": windows, "macos": macos}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("platform", choices=sorted(PLATFORMS))
    parser.add_argument(
        "--base-url",
        required=True,
        help="where the assets will be served, without a trailing slash",
    )
    args = parser.parse_args()

    base_url = args.base_url.rstrip("/")
    if not base_url.startswith("https://"):
        fail(f"--base-url must be https (got {args.base_url!r}): the updater refuses anything else")

    dist_release = pathlib.Path("dist-release")
    dist_manifest = pathlib.Path("dist-manifest")
    dist_release.mkdir(exist_ok=True)
    dist_manifest.mkdir(exist_ok=True)
    PLATFORMS[args.platform](os.environ["VERSION"], base_url, dist_release, dist_manifest)


if __name__ == "__main__":
    main()
