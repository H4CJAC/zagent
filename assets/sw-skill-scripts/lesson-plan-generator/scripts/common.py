#!/usr/bin/env python3
from __future__ import annotations

import json
import mimetypes
import os
import sys
from pathlib import Path
from typing import Any


def get_env(name: str, default: str | None = None) -> str | None:
    value = os.environ.get(name)
    if value is None or value == "":
        return default
    return value


def resolve_path(file_path: str) -> Path:
    return Path(file_path).expanduser().resolve()


def infer_mime_type(file_path: str | Path) -> str:
    extension_map = {
        ".md": "text/markdown",
        ".txt": "text/plain",
        ".json": "application/json",
        ".html": "text/html",
        ".htm": "text/html",
        ".png": "image/png",
        ".jpg": "image/jpeg",
        ".jpeg": "image/jpeg",
        ".mp3": "audio/mpeg",
        ".mp4": "video/mp4",
        ".pdf": "application/pdf",
    }
    suffix = Path(file_path).suffix.lower()
    if suffix in extension_map:
        return extension_map[suffix]
    guessed, _ = mimetypes.guess_type(str(file_path))
    return guessed or "application/octet-stream"


def to_file_url(file_path: str | Path) -> str:
    return Path(file_path).resolve().as_uri()


def print_json(payload: dict[str, Any]) -> None:
    print(json.dumps(payload, ensure_ascii=False, indent=2))


def build_error_payload(message: str, **extra: Any) -> dict[str, Any]:
    payload: dict[str, Any] = {"success": False, "message": message}
    payload.update(extra)
    return payload


def exit_with_error(message: str, **extra: Any) -> int:
    print_json(build_error_payload(message, **extra))
    return 1


def ensure_file_exists(file_path: Path) -> None:
    if not file_path.is_file():
        raise FileNotFoundError(f"File not found: {file_path}")


def add_script_root_to_syspath() -> None:
    script_root = Path(__file__).resolve().parent
    if str(script_root) not in sys.path:
        sys.path.insert(0, str(script_root))
