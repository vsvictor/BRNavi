from __future__ import annotations

import hashlib
import http.client
import json
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from server import ARTIFACTS, make_handler


class OfflinePackageServerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = tempfile.TemporaryDirectory()
        self.data_root = Path(self.temp_dir.name)
        self.package_dir = self.data_root / "regions" / "sample-region" / "test-1"
        self.package_dir.mkdir(parents=True)
        files = {"tiles": b"tile archive bytes", "addressIndex": b"sqlite index bytes"}
        artifacts = {}
        for key, data in files.items():
            definition = ARTIFACTS[key]
            (self.package_dir / definition["fileName"]).write_bytes(data)
            artifacts[key] = {
                **definition,
                "bytes": len(data),
                "sha256": hashlib.sha256(data).hexdigest(),
                "url": f"/v1/packages/sample-region/test-1/{definition['fileName']}",
            }
        manifest = {
            "schemaVersion": 1,
            "regionId": "sample-region",
            "displayName": "Sample Region",
            "version": "test-1",
            "artifacts": artifacts,
        }
        (self.package_dir / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), make_handler(self.data_root))
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.temp_dir.cleanup()

    def request(self, method: str, path: str, headers: dict[str, str] | None = None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=3)
        try:
            connection.request(method, path, headers=headers or {})
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def test_health_catalog_and_manifest(self) -> None:
        status, _, body = self.request("GET", "/healthz")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {"status": "ok"})

        status, _, body = self.request("GET", "/v1/catalog")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["regions"][0]["id"], "sample-region")

        status, _, body = self.request("GET", "/v1/regions/sample-region/test-1/manifest.json")
        self.assertEqual(status, 200)
        manifest = json.loads(body)
        self.assertEqual(manifest["displayName"], "Sample Region")
        self.assertEqual(set(manifest["artifacts"]), {"tiles", "addressIndex"})

    def test_artifact_download_and_byte_range(self) -> None:
        path = "/v1/packages/sample-region/test-1/tiles.tar.gz"
        status, headers, body = self.request("GET", path)
        self.assertEqual(status, 200)
        self.assertEqual(headers["Accept-Ranges"], "bytes")
        self.assertEqual(headers["ETag"], f'"sha256-{hashlib.sha256(body).hexdigest()}"')

        status, headers, partial = self.request("GET", path, {"Range": "bytes=2-6"})
        self.assertEqual(status, 206)
        self.assertEqual(partial, body[2:7])
        self.assertEqual(headers["Content-Range"], f"bytes 2-6/{len(body)}")

    def test_head_and_unsafe_paths(self) -> None:
        status, headers, body = self.request(
            "HEAD", "/v1/packages/sample-region/test-1/address-index.sqlite"
        )
        self.assertEqual(status, 200)
        self.assertEqual(body, b"")
        self.assertGreater(int(headers["Content-Length"]), 0)

        status, _, _ = self.request("GET", "/v1/packages/../../README.md")
        self.assertEqual(status, 404)


if __name__ == "__main__":
    unittest.main()
