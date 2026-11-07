#!/usr/bin/env python3
"""Stage a copy of the plugin store for the R2 mirror.

    mirror-plugins.py --out DIR [--registry-url URL] [--published URL]
                      [--allow-removal]

Reads the merged registry (public and private plugins, the copy only
waveflow.app serves), downloads every archive it names, and checks each
one the way the app's store does before it would install it: the blake3
of `plugin.wasm` is the one the registry pins, the manifest names the
entry's id, version and world, and `plugin.wasm` is a component, not a
core module (`plugin::binfmt`). It then writes, under DIR:

    plugins/<id>/<id>-v<version>.zip   each archive, as downloaded
    plugins/registry.json              the registry, byte for byte

and nothing else; `publish-plugins-r2.sh` uploads that tree. Nothing is
written unless every archive passed: the mirror's registry never names
an archive the mirror does not hold, nor one the app would refuse.

The app checks each archive it downloads, from either host, against
the registry it read; this script only keeps the mirror from holding
what that check would refuse. The registry copy it publishes is, for an
app that cannot reach waveflow.app, the registry itself: what this
script uploads there is trusted, so it copies the bytes waveflow.app
served and nothing else.

Refused outright, with nothing written (exit 1):
  - a registry that does not answer, is not JSON, declares a
    `schema_version` other than 1, or lists no plugin;
  - an entry whose id, version or blake3 could not name a file safely;
  - with --published, a registry that drops a plugin the mirror's
    current copy lists, unless --allow-removal: an outage that served
    half the catalogue must not shrink the good copy. A withdrawn plugin
    is a deliberate act, so it is a deliberate run.

The archives are downloaded from the entry's `download_url`, or from its
GitHub release as the app builds that address (`asset`, or
`<id>-v<version>.zip`). Only https is followed, redirects included, and
never to localhost or a loopback, private, link-local or otherwise
internal IP literal — the app's `is_safe_motion_url`, on a runner that
has a metadata endpoint at 169.254.169.254. A host name is not resolved
first, as the app does not resolve it either. No address is printed
with its query string.

`blake3` is the PyPI package; it is imported when a hash is needed, so
the tests run without it.
"""

from __future__ import annotations

import argparse
import http.client
import io
import ipaddress
import json
import re
import socket
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path
from typing import Callable

REGISTRY_URL = "https://waveflow.app/api/plugins/registry"
SUPPORTED_SCHEMA_VERSION = 1

# The app's own caps (commands/plugin_store.rs): an archive past the
# first, or a plugin.wasm past the second, is refused there too.
MAX_DOWNLOAD_BYTES = 64 * 1024 * 1024
MAX_WASM_SIZE = 50 * 1024 * 1024
MAX_MANIFEST_SIZE = 256 * 1024
# A registry is a few dozen kilobytes.
MAX_REGISTRY_BYTES = 4 * 1024 * 1024

USER_AGENT = "WaveFlow-plugin-mirror/1 (+https://github.com/InstaZDLL/WaveFlow)"
TIMEOUT = 60

# The app's `validate_plugin_id_chars`: the id is a path segment on the
# mirror and a directory name on disk.
ID_RE = re.compile(r"[a-z0-9-]+")
# What a version may be to sit in a file name and a URL path segment.
VERSION_RE = re.compile(r"[0-9A-Za-z][0-9A-Za-z.+-]*")
BLAKE3_RE = re.compile(r"[0-9a-fA-F]{64}")


class Refused(Exception):
    """A reason to write nothing at all."""


def redact(url: str) -> str:
    """The address without its userinfo, query or fragment."""
    parts = urllib.parse.urlsplit(url)
    host = parts.hostname or ""
    if parts.port:
        host = f"{host}:{parts.port}"
    masked = "?…" if parts.query else ""
    return urllib.parse.urlunsplit((parts.scheme, host, parts.path, "", "")) + masked


def is_safe_url(url: str) -> bool:
    """https, to a host that is neither localhost nor an internal IP
    literal: the app's `is_safe_motion_url`."""
    try:
        parts = urllib.parse.urlsplit(url)
        host = parts.hostname
    except ValueError:
        return False
    if parts.scheme != "https" or not host:
        return False
    host = host.rstrip(".").lower()
    if not host or host == "localhost" or host.endswith(".localhost"):
        return False
    try:
        ip = ipaddress.ip_address(host)
    except ValueError:
        # The shorthand IPv4 forms the resolver still takes as an address
        # (2130706433, 0x7f.1, 127.1): an address, not a name.
        try:
            ip = ipaddress.IPv4Address(socket.inet_aton(host))
        except OSError:
            return True  # a name, not resolved here, as in the app
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped:
        ip = ip.ipv4_mapped
    return ip.is_global and not ip.is_multicast


class _SafeRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not is_safe_url(newurl):
            raise urllib.error.HTTPError(
                redact(newurl), code, "redirect to an unsafe address refused", headers, fp
            )
        return super().redirect_request(req, fp, code, msg, headers, newurl)


_opener = urllib.request.build_opener(_SafeRedirects())


def fetch(url: str, cap: int) -> bytes:
    """GET `url`, refusing a body past `cap` bytes. Raises on any failure,
    a status other than 2xx included (urllib raises HTTPError for it)."""
    if not is_safe_url(url):
        raise Refused(f"{redact(url)}: not a safe https address")
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with _opener.open(req, timeout=TIMEOUT) as resp:
        length = resp.headers.get("Content-Length")
        if length is not None and length.isdigit() and int(length) > cap:
            raise Refused(f"{redact(url)}: {length} bytes, more than {cap}")
        body = resp.read(cap + 1)
        if len(body) > cap:
            raise Refused(f"{redact(url)}: more than {cap} bytes")
        if length is not None and length.isdigit() and len(body) != int(length):
            raise Refused(f"{redact(url)}: {len(body)} of {length} bytes, truncated")
        return body


def parse_registry(raw: bytes) -> dict:
    """The registry, or Refused. Checks what the mirror relies on: the
    schema the app reads, at least one plugin, and entries whose names
    can become file names."""
    try:
        reg = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as e:
        raise Refused(f"the registry is not JSON: {e}") from None
    if not isinstance(reg, dict):
        raise Refused("the registry is not a JSON object")
    version = reg.get("schema_version")
    if type(version) is not int or version != SUPPORTED_SCHEMA_VERSION:
        raise Refused(
            f"the registry declares schema_version {version!r}, "
            f"this mirror copies {SUPPORTED_SCHEMA_VERSION}"
        )
    plugins = reg.get("plugins")
    if not isinstance(plugins, list) or not plugins:
        raise Refused("the registry lists no plugin")
    seen = set()
    for i, entry in enumerate(plugins):
        if not isinstance(entry, dict):
            raise Refused(f"registry entry {i} is not an object")
        pid = entry.get("id")
        if not isinstance(pid, str) or not ID_RE.fullmatch(pid):
            raise Refused(f"registry entry {i}: id {pid!r} is not [a-z0-9-]+")
        if pid in seen:
            raise Refused(f"registry lists {pid} twice")
        seen.add(pid)
        ver = entry.get("version")
        if not isinstance(ver, str) or not VERSION_RE.fullmatch(ver):
            raise Refused(f"{pid}: version {ver!r} cannot name a file")
        digest = entry.get("blake3")
        if not isinstance(digest, str) or not BLAKE3_RE.fullmatch(digest):
            raise Refused(f"{pid}: blake3 {digest!r} is not 64 hex digits")
        for key in ("world", "repo"):
            if not isinstance(entry.get(key), str):
                raise Refused(f"{pid}: {key} is missing")
    return reg


def archive_url(entry: dict) -> str:
    """Where the app downloads this entry from: `download_url` when the
    registry gives one, else the GitHub release built from `repo`."""
    direct = entry.get("download_url")
    if direct is not None:
        if not isinstance(direct, str) or not is_safe_url(direct):
            raise Refused(f"{entry['id']}: download_url is not a safe https address")
        return direct
    asset = entry.get("asset") or f"{entry['id']}-v{entry['version']}.zip"
    return (
        f"https://github.com/{entry['repo']}/releases/download/"
        f"v{entry['version']}/{asset}"
    )


def mirror_key(entry: dict) -> str:
    """The archive's path in the bucket — the address the app falls back
    to (`plugin_store::mirror_archive_url`)."""
    pid, ver = entry["id"], entry["version"]
    return f"plugins/{pid}/{pid}-v{ver}.zip"


def blake3_hex(data: bytes) -> str:
    try:
        import blake3  # type: ignore[import-not-found]
    except ImportError:
        raise Refused("the blake3 package is missing: pip install blake3") from None
    return blake3.blake3(data).hexdigest()


def _member(archive: zipfile.ZipFile, name: str, cap: int) -> bytes | None:
    try:
        info = archive.getinfo(name)
    except KeyError:
        return None
    if info.file_size > cap:
        raise Refused(f"{name} is {info.file_size} bytes, more than {cap}")
    with archive.open(info) as f:
        data = f.read(cap + 1)
    if len(data) > cap:
        raise Refused(f"{name} is more than {cap} bytes")
    return data


def is_component(wasm: bytes) -> bool:
    """`plugin::binfmt::classify_wasm_binary` answering `Component`: the
    wasm magic, then layer 1 in bytes 6-7 (little-endian), whatever the
    version in bytes 4-5. A core module (version 1, layer 0) is valid wasm
    the host can never load."""
    return len(wasm) >= 8 and wasm[:4] == b"\0asm" and int.from_bytes(wasm[6:8], "little") == 1


def verify_archive(
    data: bytes, entry: dict, hasher: Callable[[bytes], str] = blake3_hex
) -> None:
    """Refused unless the app would accept these bytes for this entry:
    a zip whose plugin.wasm hashes to the registry's blake3 and is a
    component, and whose manifest names the entry's id, version and
    world."""
    pid = entry["id"]
    try:
        archive = zipfile.ZipFile(io.BytesIO(data))
    except zipfile.BadZipFile:
        raise Refused(f"{pid}: the download is not a zip archive") from None
    with archive:
        wasm = _member(archive, "plugin.wasm", MAX_WASM_SIZE)
        if wasm is None:
            raise Refused(f"{pid}: the archive has no plugin.wasm")
        actual = hasher(wasm)
        expected = entry["blake3"].lower()
        if actual != expected:
            raise Refused(
                f"{pid}: blake3 mismatch, the registry pins {expected}, "
                f"the archive's plugin.wasm is {actual}"
            )
        manifest_bytes = _member(archive, "manifest.toml", MAX_MANIFEST_SIZE)
    if not is_component(wasm):
        raise Refused(f"{pid}: plugin.wasm is not a WebAssembly component; the app refuses it")
    if manifest_bytes is None:
        raise Refused(f"{pid}: the archive has no manifest.toml")
    try:
        manifest = tomllib.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
        raise Refused(f"{pid}: manifest.toml does not parse: {e}") from None
    plugin = manifest.get("plugin") if isinstance(manifest, dict) else None
    if not isinstance(plugin, dict):
        raise Refused(f"{pid}: manifest.toml has no [plugin] table")
    for key in ("id", "version", "world"):
        if plugin.get(key) != entry[key]:
            raise Refused(
                f"{pid}: manifest {key} {plugin.get(key)!r} is not the registry's {entry[key]!r}"
            )


def removed_ids(published: dict | None, new: dict) -> list[str]:
    """The ids the mirror's current registry lists and the new one drops."""
    if published is None:
        return []
    old = {
        e.get("id")
        for e in published.get("plugins", [])
        if isinstance(e, dict) and isinstance(e.get("id"), str)
    }
    kept = {e["id"] for e in new["plugins"]}
    return sorted(old - kept)


def read_published(url: str) -> dict | None:
    """The registry the mirror serves now, None before the first run.
    Anything else that cannot be read stops the run: without it, the
    removal guard would be guessing."""
    try:
        raw = fetch(url, MAX_REGISTRY_BYTES)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise Refused(f"could not read {redact(url)}: HTTP {e.code}") from None
    except (urllib.error.URLError, http.client.HTTPException, OSError) as e:
        raise Refused(f"could not read {redact(url)}: {e}") from None
    try:
        reg = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise Refused(f"{redact(url)} is not JSON; run with --allow-removal to replace it") from None
    if not isinstance(reg, dict):
        raise Refused(f"{redact(url)} is not a registry; run with --allow-removal to replace it")
    return reg


def stage(
    raw_registry: bytes,
    registry: dict,
    out: Path,
    download: Callable[[str], bytes],
    hasher: Callable[[bytes], str] = blake3_hex,
) -> list[str]:
    """Download and verify every archive, then write the tree. Returns
    the bucket keys written, archives first and the registry last."""
    staged: list[tuple[str, bytes]] = []
    failures: list[str] = []
    for entry in registry["plugins"]:
        url = archive_url(entry)
        try:
            data = download(url)
            verify_archive(data, entry, hasher)
        except Refused as e:
            failures.append(str(e))
            continue
        except (urllib.error.URLError, http.client.HTTPException, OSError) as e:
            failures.append(f"{entry['id']}: {redact(url)}: {e}")
            continue
        print(f"verified {entry['id']} {entry['version']} ({len(data)} bytes)")
        staged.append((mirror_key(entry), data))
    if failures:
        raise Refused(
            f"{len(failures)} of {len(registry['plugins'])} archives refused, "
            "nothing staged:\n  " + "\n  ".join(failures)
        )
    keys = []
    for key, data in staged:
        path = out / key
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        keys.append(key)
    (out / "plugins").mkdir(parents=True, exist_ok=True)
    (out / "plugins" / "registry.json").write_bytes(raw_registry)
    keys.append("plugins/registry.json")
    return keys


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--out", required=True, type=Path, help="empty directory to stage into")
    ap.add_argument("--registry-url", default=REGISTRY_URL)
    ap.add_argument(
        "--published",
        help="the mirror's current registry.json, for the removal guard",
    )
    ap.add_argument(
        "--allow-removal",
        action="store_true",
        help="publish even if plugins the mirror lists are gone from the registry",
    )
    args = ap.parse_args(argv)

    try:
        blake3_hex(b"")  # fail before the downloads, not after each one
    except Refused as e:
        print(f"::error::{e}", file=sys.stderr)
        return 1
    if args.out.exists() and any(args.out.iterdir()):
        print(f"::error::{args.out} is not empty", file=sys.stderr)
        return 1
    try:
        try:
            raw = fetch(args.registry_url, MAX_REGISTRY_BYTES)
        except urllib.error.HTTPError as e:
            raise Refused(f"{redact(args.registry_url)}: HTTP {e.code}") from None
        except (urllib.error.URLError, http.client.HTTPException, OSError) as e:
            raise Refused(f"{redact(args.registry_url)}: {e}") from None
        registry = parse_registry(raw)
        print(f"registry: {len(registry['plugins'])} plugins")

        if args.published and not args.allow_removal:
            gone = removed_ids(read_published(args.published), registry)
            if gone:
                raise Refused(
                    "the registry no longer lists "
                    + ", ".join(gone)
                    + "; if that is a withdrawal, run again with --allow-removal"
                )

        keys = stage(raw, registry, args.out, lambda u: fetch(u, MAX_DOWNLOAD_BYTES))
    except Refused as e:
        print(f"::error::{e}", file=sys.stderr)
        print("Nothing staged: the mirror stays as it is.", file=sys.stderr)
        return 1
    print(f"staged {len(keys)} files under {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
