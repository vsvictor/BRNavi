#!/usr/bin/env python3
"""Read-only HTTP server for versioned offline-map packages."""

from __future__ import annotations

import argparse
import json
import logging
import os
import re
import stat
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
REGION_ID_RE = re.compile(r"^[a-z0-9][a-z0-9-]{0,63}$")
VERSION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._+-]{0,63}$")
ARTIFACTS = {
    "tiles": {
        "fileName": "tiles.tar.gz",
        "mediaType": "application/gzip",
        "format": "valhalla-tile-archive",
    },
    "addressIndex": {
        "fileName": "address-index.sqlite",
        "mediaType": "application/vnd.sqlite3",
        "format": "sqlite-fts5",
    },
}
SHA256_RE = re.compile(r"^[a-f0-9]{64}$")
RANGE_RE = re.compile(r"^bytes=(\d*)-(\d*)$")
LOGGER = logging.getLogger("offline-region-server")


class PackageError(Exception):
    """Package metadata or files are missing, unsafe, or inconsistent."""


def _valid_region_id(value: str) -> bool:
    return bool(REGION_ID_RE.fullmatch(value))


def _valid_version(value: str) -> bool:
    return bool(VERSION_RE.fullmatch(value)) and value not in {".", ".."}


def _safe_existing_path(root: Path, path: Path, *, expect_directory: bool = False) -> os.stat_result:
    """Reject symlink components and paths outside the configured data root."""
    try:
        relative = path.relative_to(root)
    except ValueError as error:
        raise PackageError("Package path is outside the data directory") from error

    current = root
    result: os.stat_result | None = None
    for index, part in enumerate(relative.parts):
        current = current / part
        try:
            result = current.lstat()
        except FileNotFoundError as error:
            raise PackageError("Package file does not exist") from error
        if stat.S_ISLNK(result.st_mode):
            raise PackageError("Symlinks are not allowed in package paths")
        is_last = index == len(relative.parts) - 1
        if not is_last and not stat.S_ISDIR(result.st_mode):
            raise PackageError("Package path component is not a directory")

    if result is None:
        result = root.lstat()
    if expect_directory:
        if not stat.S_ISDIR(result.st_mode):
            raise PackageError("Package path is not a directory")
    elif not stat.S_ISREG(result.st_mode):
        raise PackageError("Package path is not a regular file")
    return result


def _open_regular_file(root: Path, path: Path) -> tuple[int, os.stat_result]:
    _safe_existing_path(root, path)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise PackageError("Package file cannot be opened safely") from error
    result = os.fstat(descriptor)
    if not stat.S_ISREG(result.st_mode):
        os.close(descriptor)
        raise PackageError("Package path is not a regular file")
    return descriptor, result


def _canonical_urls(region_id: str, version: str) -> dict[str, str]:
    base = f"/v1/packages/{region_id}/{version}"
    return {
        key: f"{base}/{artifact['fileName']}"
        for key, artifact in ARTIFACTS.items()
    }


def _read_manifest(data_root: Path, region_id: str, version: str) -> dict[str, Any]:
    package_dir = data_root / "regions" / region_id / version
    try:
        _safe_existing_path(data_root, package_dir, expect_directory=True)
        path = package_dir / "manifest.json"
        descriptor, _ = _open_regular_file(data_root, path)
        with os.fdopen(descriptor, "rb") as stream:
            raw = stream.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise PackageError("Manifest exceeds the 1 MiB limit")
        document = json.loads(raw.decode("utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        raise PackageError("Manifest is unavailable or invalid") from error

    if not isinstance(document, dict) or document.get("schemaVersion") != SCHEMA_VERSION:
        raise PackageError("Unsupported manifest schema")
    if (
        document.get("regionId") != region_id
        or document.get("version") != version
        or not isinstance(document.get("displayName"), str)
        or not document["displayName"].strip()
        or len(document["displayName"]) > 200
    ):
        raise PackageError("Manifest identity does not match its package path")

    raw_artifacts = document.get("artifacts")
    if not isinstance(raw_artifacts, dict):
        raise PackageError("Manifest artifacts are missing")
    urls = _canonical_urls(region_id, version)
    artifacts: dict[str, dict[str, Any]] = {}
    for key, definition in ARTIFACTS.items():
        metadata = raw_artifacts.get(key)
        if not isinstance(metadata, dict):
            raise PackageError(f"Manifest artifact {key} is missing")
        size = metadata.get("bytes")
        digest = metadata.get("sha256")
        if type(size) is not int or size <= 0 or not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            raise PackageError(f"Manifest artifact {key} has invalid integrity metadata")
        if (
            metadata.get("fileName") != definition["fileName"]
            or metadata.get("mediaType") != definition["mediaType"]
            or metadata.get("format") != definition["format"]
            or metadata.get("url") != urls[key]
        ):
            raise PackageError(f"Manifest artifact {key} has invalid package metadata")

        artifact_path = package_dir / definition["fileName"]
        try:
            artifact_stat = _safe_existing_path(data_root, artifact_path)
        except PackageError as error:
            raise PackageError(f"Manifest artifact {key} is unavailable") from error
        if artifact_stat.st_size != size:
            raise PackageError(f"Manifest artifact {key} size does not match its file")
        artifacts[key] = {
            **definition,
            "bytes": size,
            "sha256": digest,
            "url": urls[key],
        }

    return {
        "schemaVersion": SCHEMA_VERSION,
        "regionId": region_id,
        "displayName": document["displayName"],
        "version": version,
        "artifacts": artifacts,
    }


def _response_json(handler: BaseHTTPRequestHandler, status: int, payload: dict[str, Any], *, cache: str = "no-store") -> None:
    body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    handler.send_response(status)
    handler.send_header("Content-Type", "application/json; charset=utf-8")
    handler.send_header("Content-Length", str(len(body)))
    handler.send_header("Cache-Control", cache)
    handler.send_header("X-Content-Type-Options", "nosniff")
    handler.end_headers()
    if handler.command != "HEAD":
        handler.wfile.write(body)


def _parse_byte_range(value: str, size: int) -> tuple[int, int] | None:
    """Return an inclusive single byte range, None when the header is ignored."""
    match = RANGE_RE.fullmatch(value.strip())
    if match is None or "," in value:
        return None
    first, last = match.groups()
    if not first and not last:
        return None
    if not first:
        suffix_length = int(last)
        if suffix_length <= 0:
            raise IndexError
        return max(size - suffix_length, 0), size - 1
    start = int(first)
    end = int(last) if last else size - 1
    if start >= size or end < start:
        raise IndexError
    return start, min(end, size - 1)


def make_handler(data_directory: Path) -> type[BaseHTTPRequestHandler]:
    data_root = data_directory.resolve()

    class OfflineRegionRequestHandler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = "OfflineRegionServer/1"

        def do_GET(self) -> None:
            self._dispatch()

        def do_HEAD(self) -> None:
            self._dispatch()

        def _dispatch(self) -> None:
            try:
                path = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path, errors="strict")
            except (UnicodeDecodeError, ValueError):
                _response_json(self, 400, {"error": {"code": "bad_request", "message": "Invalid request path"}})
                return

            if path == "/healthz":
                _response_json(self, 200, {"status": "ok"})
                return
            if path == "/v1/catalog":
                self._catalog()
                return

            manifest_match = re.fullmatch(
                r"/v1/regions/([^/]+)/([^/]+)/manifest\.json", path
            )
            if manifest_match:
                region_id, version = manifest_match.groups()
                if not _valid_region_id(region_id) or not _valid_version(version):
                    self._not_found()
                    return
                try:
                    manifest = _read_manifest(data_root, region_id, version)
                except PackageError:
                    _response_json(self, 503, {"error": {"code": "package_unavailable", "message": "Package metadata is unavailable"}})
                    return
                _response_json(
                    self,
                    200,
                    manifest,
                    cache="public, max-age=31536000, immutable",
                )
                return

            artifact_match = re.fullmatch(
                r"/v1/packages/([^/]+)/([^/]+)/([^/]+)", path
            )
            if artifact_match:
                region_id, version, file_name = artifact_match.groups()
                artifact_key = next(
                    (key for key, value in ARTIFACTS.items() if value["fileName"] == file_name),
                    None,
                )
                if not _valid_region_id(region_id) or not _valid_version(version) or artifact_key is None:
                    self._not_found()
                    return
                self._artifact(region_id, version, artifact_key)
                return

            self._not_found()

        def _catalog(self) -> None:
            catalog_regions: list[dict[str, str]] = []
            regions_path = data_root / "regions"
            if regions_path.exists() and not regions_path.is_symlink():
                try:
                    _safe_existing_path(data_root, regions_path, expect_directory=True)
                    region_entries = sorted(regions_path.iterdir(), key=lambda item: item.name)
                except (PackageError, OSError):
                    region_entries = []
                for region_path in region_entries:
                    region_id = region_path.name
                    if not _valid_region_id(region_id) or region_path.is_symlink() or not region_path.is_dir():
                        continue
                    try:
                        versions = sorted(region_path.iterdir(), key=lambda item: item.name)
                    except OSError:
                        continue
                    for version_path in versions:
                        version = version_path.name
                        if not _valid_version(version) or version_path.is_symlink() or not version_path.is_dir():
                            continue
                        try:
                            manifest = _read_manifest(data_root, region_id, version)
                        except PackageError as error:
                            LOGGER.warning("Skipping unavailable package %s/%s: %s", region_id, version, error)
                            continue
                        catalog_regions.append(
                            {
                                "id": manifest["regionId"],
                                "displayName": manifest["displayName"],
                                "version": manifest["version"],
                                "manifestUrl": f"/v1/regions/{region_id}/{version}/manifest.json",
                            }
                        )
            _response_json(
                self,
                200,
                {"schemaVersion": SCHEMA_VERSION, "regions": catalog_regions},
                cache="public, max-age=60",
            )

        def _artifact(self, region_id: str, version: str, artifact_key: str) -> None:
            try:
                manifest = _read_manifest(data_root, region_id, version)
                artifact = manifest["artifacts"][artifact_key]
                file_path = data_root / "regions" / region_id / version / artifact["fileName"]
                descriptor, file_stat = _open_regular_file(data_root, file_path)
            except PackageError:
                _response_json(self, 503, {"error": {"code": "package_unavailable", "message": "Package artifact is unavailable"}})
                return

            size = file_stat.st_size
            etag = f'"sha256-{artifact["sha256"]}"'
            byte_range: tuple[int, int] | None = None
            range_header = self.headers.get("Range")
            if range_header:
                if_range = self.headers.get("If-Range")
                if if_range is None or if_range.strip() == etag:
                    try:
                        byte_range = _parse_byte_range(range_header, size)
                    except (IndexError, ValueError):
                        os.close(descriptor)
                        self.send_response(416)
                        self.send_header("Content-Range", f"bytes */{size}")
                        self.send_header("Content-Length", "0")
                        self.send_header("Accept-Ranges", "bytes")
                        self.send_header("Cache-Control", "public, max-age=31536000, immutable")
                        self.end_headers()
                        return

            status = 206 if byte_range is not None else 200
            start, end = byte_range if byte_range is not None else (0, size - 1)
            length = end - start + 1
            self.send_response(status)
            self.send_header("Content-Type", artifact["mediaType"])
            self.send_header("Content-Length", str(length))
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", self.date_time_string(file_stat.st_mtime))
            self.send_header("Cache-Control", "public, max-age=31536000, immutable")
            self.send_header("Content-Disposition", f'attachment; filename="{artifact["fileName"]}"')
            self.send_header("X-Content-Type-Options", "nosniff")
            if byte_range is not None:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.end_headers()
            try:
                if self.command != "HEAD":
                    os.lseek(descriptor, start, os.SEEK_SET)
                    remaining = length
                    while remaining:
                        chunk = os.read(descriptor, min(64 * 1024, remaining))
                        if not chunk:
                            break
                        self.wfile.write(chunk)
                        remaining -= len(chunk)
            except (BrokenPipeError, ConnectionResetError):
                pass
            finally:
                os.close(descriptor)

        def _not_found(self) -> None:
            _response_json(self, 404, {"error": {"code": "not_found", "message": "Resource not found"}})

        def log_message(self, format_string: str, *args: Any) -> None:
            LOGGER.info("%s - %s", self.address_string(), format_string % args)

    return OfflineRegionRequestHandler


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=Path("data"), help="Package root containing regions/ (default: ./data)")
    parser.add_argument("--host", default="127.0.0.1", help="Bind address (default: 127.0.0.1)")
    parser.add_argument("--port", type=int, default=8080, help="HTTP port (default: 8080)")
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")

    args.data_dir.mkdir(parents=True, exist_ok=True)
    server = ThreadingHTTPServer((args.host, args.port), make_handler(args.data_dir))
    server.daemon_threads = True
    LOGGER.info("Serving offline-region packages from %s on %s:%d", args.data_dir.resolve(), args.host, args.port)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    main()
