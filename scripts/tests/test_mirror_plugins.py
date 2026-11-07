"""Tests for scripts/release/mirror-plugins.py.

    python3 -m unittest discover -s scripts/tests -p 'test_*.py'

Standard library only: the archives are hashed with a stand-in, so the
blake3 package is not needed to run them.
"""

from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import tempfile
import unittest
import zipfile
from pathlib import Path

_SCRIPT = Path(__file__).resolve().parents[1] / "release" / "mirror-plugins.py"
_spec = importlib.util.spec_from_file_location("mirror_plugins", _SCRIPT)
assert _spec and _spec.loader
mp = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mp)


def fake_hash(data: bytes) -> str:
    # Any 64-hex digest will do: the tests check the comparison, not blake3.
    return hashlib.sha256(data).hexdigest()


WASM = b"\0asm\x0d\x00\x01\x00component"


def entry(pid: str = "lyrics-x", version: str = "0.1.0", **extra) -> dict:
    e = {
        "id": pid,
        "name": pid,
        "description": "d",
        "author": "a",
        "repo": "InstaZDLL/waveflow-plugin-x",
        "world": "waveflow:metadata/v3",
        "version": version,
        "blake3": fake_hash(WASM),
        "permissions": {},
    }
    e.update(extra)
    return e


def registry(*entries: dict, schema: object = 1) -> dict:
    return {"schema_version": schema, "plugins": list(entries)}


def archive(
    e: dict, wasm: bytes = WASM, manifest: dict | None = None, skip=(), schema: str = "1"
) -> bytes:
    m = manifest or {
        "id": e["id"],
        "name": e["name"],
        "version": e["version"],
        "author": e["author"],
        "world": e["world"],
    }
    toml = f"schema_version = {schema}\n[plugin]\n" + "".join(
        f'{k} = "{v}"\n' for k, v in m.items()
    )
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        if "plugin.wasm" not in skip:
            z.writestr("plugin.wasm", wasm)
        if "manifest.toml" not in skip:
            z.writestr("manifest.toml", toml)
    return buf.getvalue()


class ParseRegistry(unittest.TestCase):
    def ok(self, reg: dict) -> dict:
        return mp.parse_registry(json.dumps(reg).encode())

    def refused(self, raw: bytes) -> None:
        with self.assertRaises(mp.Refused):
            mp.parse_registry(raw)

    def test_accepts_a_registry(self):
        self.assertEqual(len(self.ok(registry(entry()))["plugins"]), 1)

    def test_refuses_what_is_not_json(self):
        self.refused(b"<html>account suspended</html>")
        self.refused(b"\xff\xfe")
        self.refused(b"[]")

    def test_refuses_another_schema(self):
        for schema in (2, 0, "1", True, None):
            with self.subTest(schema=schema):
                self.refused(json.dumps(registry(entry(), schema=schema)).encode())

    def test_refuses_an_empty_registry(self):
        self.refused(json.dumps(registry()).encode())
        self.refused(json.dumps({"schema_version": 1}).encode())

    def test_refuses_names_that_could_escape_a_path(self):
        for bad in ("../x", "x/y", "X", "", "a b"):
            with self.subTest(id=bad):
                self.refused(json.dumps(registry(entry(pid=bad))).encode())
        for bad in ("../1", "1/2", "", ".1", "1 2"):
            with self.subTest(version=bad):
                self.refused(json.dumps(registry(entry(version=bad))).encode())

    def test_accepts_every_shape_the_app_decodes(self):
        self.ok(
            registry(
                entry(
                    description={"en": "d", "fr": "d"},
                    description_i18n={"fr": "d"},
                    homepage=None,
                    asset="x.zip",
                    download_url=None,
                    min_app_version="2.0.0",
                    tags=["lyrics"],
                    official=True,
                    permissions={
                        "http": ["api.example"],
                        "storage_read": False,
                        "storage_state": True,
                        "library_read_artists": False,
                    },
                )
            )
        )

    def test_refuses_an_entry_the_app_could_not_decode(self):
        for bad in (
            {"name": None},
            {"author": 3},
            {"description": None},
            {"description": {"fr": 1}},
            {"description_i18n": ["fr"]},
            {"homepage": 1},
            {"tags": None},
            {"tags": ["a", 1]},
            {"official": "yes"},
            {"permissions": None},
            {"permissions": {"http": "api.example"}},
            {"permissions": {"storage_state": 1}},
        ):
            with self.subTest(bad=bad):
                self.refused(json.dumps(registry(entry(**bad))).encode())
        without_permissions = entry()
        del without_permissions["permissions"]
        self.refused(json.dumps(registry(without_permissions)).encode())

    def test_refuses_a_key_given_twice(self):
        good = json.dumps(registry(entry()))
        pin = json.dumps(entry()["blake3"])
        for raw in (
            # A struct field: serde refuses the whole registry.
            good.replace('"blake3": ', '"blake3": ' + pin + ', "blake3": ', 1),
            good.replace('"schema_version": 1', '"schema_version": 1, "schema_version": 1', 1),
            # A permissions field, one object deeper.
            good.replace('"permissions": {}', '"permissions": {"http": [], "http": []}', 1),
        ):
            with self.subTest(raw=raw[:60]), self.assertRaisesRegex(mp.Refused, "twice"):
                mp.parse_registry(raw.encode())

    def test_refuses_a_malformed_pin_or_a_duplicate(self):
        self.refused(json.dumps(registry(entry(blake3="abc"))).encode())
        self.refused(json.dumps(registry(entry(), entry())).encode())


class ArchiveUrl(unittest.TestCase):
    def test_download_url_wins(self):
        url = "https://waveflow.app/plugins/x/x-v1.zip"
        self.assertEqual(mp.archive_url(entry(download_url=url)), url)

    def test_github_release_as_the_app_builds_it(self):
        self.assertEqual(
            mp.archive_url(entry()),
            "https://github.com/InstaZDLL/waveflow-plugin-x/releases/download/v0.1.0/lyrics-x-v0.1.0.zip",
        )
        self.assertTrue(mp.archive_url(entry(asset="other.zip")).endswith("/v0.1.0/other.zip"))

    def test_refuses_a_plain_http_download_url(self):
        with self.assertRaises(mp.Refused):
            mp.archive_url(entry(download_url="http://waveflow.app/x.zip"))

    def test_refuses_an_internal_download_url(self):
        for url in (
            "https://localhost/x.zip",
            "https://a.localhost/x.zip",
            "https://127.0.0.1/x.zip",
            "https://169.254.169.254/latest/meta-data",
            "https://10.0.0.1/x.zip",
            "https://[::1]/x.zip",
            "https://[::ffff:192.168.1.1]/x.zip",
            "https://100.64.0.1/x.zip",
            "https://user@127.0.0.1./x.zip",
            "https://2130706433/x.zip",
            "https://0x7f.1/x.zip",
            "https://127.1/x.zip",
            "https://2852039166/latest/meta-data",
        ):
            with self.subTest(url=url), self.assertRaises(mp.Refused):
                mp.archive_url(entry(download_url=url))

    def test_a_public_address_is_safe(self):
        for url in (
            "https://waveflow.app/plugins/x.zip",
            "https://objects.githubusercontent.com/a?b=c",
            "https://1.1.1.1/x.zip",
        ):
            with self.subTest(url=url):
                self.assertTrue(mp.is_safe_url(url))

    def test_mirror_key(self):
        self.assertEqual(mp.mirror_key(entry()), "plugins/lyrics-x/lyrics-x-v0.1.0.zip")

    def test_redact_drops_query_and_userinfo(self):
        self.assertEqual(
            mp.redact("https://user:pw@host.example/a/b.zip?token=s3cret#f"),
            "https://host.example/a/b.zip?…",
        )


class VerifyArchive(unittest.TestCase):
    def test_accepts_the_pinned_archive(self):
        e = entry()
        mp.verify_archive(archive(e), e, fake_hash)

    def test_pin_compares_case_insensitively(self):
        e = entry()
        e["blake3"] = e["blake3"].upper()
        mp.verify_archive(archive(e), e, fake_hash)

    def test_refuses_another_wasm(self):
        e = entry()
        with self.assertRaisesRegex(mp.Refused, "blake3 mismatch"):
            mp.verify_archive(archive(e, wasm=b"\0asmother"), e, fake_hash)

    def test_refuses_a_core_module(self):
        core = b"\0asm\x01\x00\x00\x00module"
        e = entry(blake3=fake_hash(core))
        with self.assertRaisesRegex(mp.Refused, "not a WebAssembly component"):
            mp.verify_archive(archive(e, wasm=core), e, fake_hash)

    def test_component_test_follows_binfmt(self):
        self.assertTrue(mp.is_component(b"\0asm\x0d\x00\x01\x00"))
        self.assertTrue(mp.is_component(b"\0asm\x0e\x00\x01\x00"))  # any version, layer 1
        self.assertFalse(mp.is_component(b"\0asm\x01\x00\x00\x00"))
        self.assertFalse(mp.is_component(b"\0asm\x0d\x00"))
        self.assertFalse(mp.is_component(b"PK\x03\x04\x0d\x00\x01\x00"))

    def test_refuses_a_page_instead_of_a_zip(self):
        with self.assertRaisesRegex(mp.Refused, "not a zip"):
            mp.verify_archive(b"<html>captive portal</html>", entry(), fake_hash)

    def test_refuses_a_missing_member(self):
        e = entry()
        for member in ("plugin.wasm", "manifest.toml"):
            with self.subTest(member=member), self.assertRaises(mp.Refused):
                mp.verify_archive(archive(e, skip=(member,)), e, fake_hash)

    def test_refuses_a_manifest_naming_something_else(self):
        e = entry()
        for key, value in (("id", "other"), ("version", "9.9.9"), ("world", "waveflow:source/v1")):
            m = {"id": e["id"], "name": "n", "version": e["version"], "author": "a"}
            m["world"] = e["world"]
            m[key] = value
            with self.subTest(key=key), self.assertRaises(mp.Refused):
                mp.verify_archive(archive(e, manifest=m), e, fake_hash)

    def test_refuses_a_manifest_the_host_does_not_read(self):
        e = entry()
        for schema in ("2", '"1"'):
            with self.subTest(schema=schema), self.assertRaisesRegex(mp.Refused, "schema_version"):
                mp.verify_archive(archive(e, schema=schema), e, fake_hash)
        for missing in ("name", "author"):
            m = {"id": e["id"], "name": "n", "version": e["version"], "author": "a", "world": e["world"]}
            del m[missing]
            with self.subTest(missing=missing), self.assertRaisesRegex(mp.Refused, missing):
                mp.verify_archive(archive(e, manifest=m), e, fake_hash)


class RemovalGuard(unittest.TestCase):
    def test_first_run_has_nothing_to_lose(self):
        self.assertEqual(mp.removed_ids(None, registry(entry())), [])

    def test_names_what_the_new_registry_drops(self):
        old = registry(entry("a"), entry("b"), entry("c"))
        self.assertEqual(mp.removed_ids(old, registry(entry("b"))), ["a", "c"])

    def test_an_addition_or_a_bump_is_not_a_removal(self):
        old = registry(entry("a"))
        new = registry(entry("a", version="0.2.0"), entry("b"))
        self.assertEqual(mp.removed_ids(old, new), [])


class ReadPublished(unittest.TestCase):
    """What the removal guard compares against. A 404 is the only answer
    that turns it off: anything else unreadable stops the run."""

    URL = "https://download.example/plugins/registry.json"

    def answer(self, fetch):
        original = mp.fetch
        mp.fetch = fetch
        self.addCleanup(setattr, mp, "fetch", original)

    def test_a_404_is_the_first_run(self):
        error = mp.urllib.error.HTTPError(self.URL, 404, "Not Found", {}, io.BytesIO())
        self.addCleanup(error.close)

        def fetch(url, cap):
            raise error

        self.answer(fetch)
        self.assertIsNone(mp.read_published(self.URL))

    def test_a_server_error_stops_the_run(self):
        error = mp.urllib.error.HTTPError(self.URL, 503, "Unavailable", {}, io.BytesIO())
        self.addCleanup(error.close)

        def fetch(url, cap):
            raise error

        self.answer(fetch)
        with self.assertRaises(mp.Refused):
            mp.read_published(self.URL)

    def test_a_network_error_stops_the_run(self):
        def fetch(url, cap):
            raise mp.urllib.error.URLError("connection refused")

        self.answer(fetch)
        with self.assertRaises(mp.Refused):
            mp.read_published(self.URL)

    def test_what_is_not_a_registry_stops_the_run(self):
        for body in (b"<html>", b"[]"):
            with self.subTest(body=body):
                self.answer(lambda url, cap, body=body: body)
                with self.assertRaises(mp.Refused):
                    mp.read_published(self.URL)

    def test_a_registry_is_read(self):
        raw = json.dumps(registry(entry("a"))).encode()
        self.answer(lambda url, cap: raw)
        self.assertEqual(mp.removed_ids(mp.read_published(self.URL), registry()), ["a"])

class Stage(unittest.TestCase):
    def test_writes_every_archive_then_the_registry_verbatim(self):
        a, b = entry("a"), entry("b", download_url="https://waveflow.app/plugins/b/b-v0.1.0.zip")
        raw = json.dumps(registry(a, b), indent=1).encode()
        served = {mp.archive_url(a): archive(a), mp.archive_url(b): archive(b)}
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            keys = mp.stage(raw, mp.parse_registry(raw), out, served.__getitem__, fake_hash)
            self.assertEqual(
                keys,
                ["plugins/a/a-v0.1.0.zip", "plugins/b/b-v0.1.0.zip", "plugins/registry.json"],
            )
            self.assertEqual((out / "plugins/registry.json").read_bytes(), raw)
            self.assertEqual((out / "plugins/b/b-v0.1.0.zip").read_bytes(), archive(b))

    def test_one_bad_archive_stages_nothing(self):
        a, b = entry("a"), entry("b")
        raw = json.dumps(registry(a, b)).encode()
        served = {mp.archive_url(a): archive(a), mp.archive_url(b): archive(b, wasm=b"tampered")}
        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            with self.assertRaisesRegex(mp.Refused, "1 of 2 archives refused"):
                mp.stage(raw, mp.parse_registry(raw), out, served.__getitem__, fake_hash)
            self.assertEqual(list(out.iterdir()), [])

    def test_a_failed_download_stages_nothing(self):
        a = entry("a")
        raw = json.dumps(registry(a)).encode()

        def download(url: str) -> bytes:
            raise OSError("connection reset")

        with tempfile.TemporaryDirectory() as d:
            out = Path(d)
            with self.assertRaises(mp.Refused):
                mp.stage(raw, mp.parse_registry(raw), out, download, fake_hash)
            self.assertEqual(list(out.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
