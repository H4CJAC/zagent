#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from urllib import error, request

SCRIPT_ROOT = Path(__file__).resolve().parent
if str(SCRIPT_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPT_ROOT))

from common import build_error_payload, get_env, print_json


DEFAULT_BASE_URL = "http://bloom-inner.seewo.com/agent-platform"


def resolve_base_url() -> str:
    return (
        get_env("LESSON_PLAN_WEB_SEARCH_BASE_URL")
        or get_env("WEB_SEARCH_BASE_URL")
        or DEFAULT_BASE_URL
    )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Search teaching resources from the web.")
    parser.add_argument("--query", required=True)
    parser.add_argument("--theme", default="")
    parser.add_argument("--limit", type=int, default=10)
    parser.add_argument("--strategy", default="FILTER")
    parser.add_argument("--article-type", default="article")
    parser.add_argument(
        "--base-url",
        default=resolve_base_url(),
    )
    return parser.parse_args(argv)


def build_request_payload(
    *,
    query: str,
    theme: str = "",
    limit: int = 10,
    strategy: str = "FILTER",
    article_type: str = "article",
) -> dict[str, object]:
    payload: dict[str, object] = {
        "query": query,
        "strategy": strategy,
        "limit": limit,
        "articleType": article_type,
    }
    if theme:
        payload["theme"] = theme
    return payload


def search_web(
    *,
    base_url: str,
    query: str,
    theme: str = "",
    limit: int = 10,
    strategy: str = "FILTER",
    article_type: str = "article",
) -> list[dict[str, object]]:
    url = f"{base_url.rstrip('/')}/api/v1/deepsearch/search"
    payload = build_request_payload(
        query=query,
        theme=theme,
        limit=limit,
        strategy=strategy,
        article_type=article_type,
    )
    body = json.dumps(payload).encode("utf-8")
    req = request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    with request.urlopen(req, timeout=30) as response:
        data = json.loads(response.read().decode("utf-8"))

    results = data.get("data", {}).get("results", [])
    normalized: list[dict[str, object]] = []
    for item in results:
        content = item.get("content", "")
        if isinstance(content, str) and len(content) > 500:
            content = f"{content[:500]}..."
        normalized.append(
            {
                "title": item.get("title", ""),
                "content": content,
                "url": item.get("url", ""),
                "isRelevant": item.get("isRelevant", False),
            }
        )
    return normalized


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        results = search_web(
            base_url=args.base_url,
            query=args.query,
            theme=args.theme,
            limit=args.limit,
            strategy=args.strategy,
            article_type=args.article_type,
        )
    except error.HTTPError as exc:
        print_json(
            build_error_payload(
                f"web search http error: {exc.code}",
                provider="web_search",
                query=args.query,
            )
        )
        return 1
    except Exception as exc:
        print_json(
            build_error_payload(
                f"web search failed: {exc}",
                provider="web_search",
                query=args.query,
            )
        )
        return 1

    print_json(
        {
            "success": True,
            "provider": "web_search",
            "query": args.query,
            "theme": args.theme,
            "count": len(results),
            "results": results,
        }
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
