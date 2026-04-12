#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from urllib import error, parse, request

SCRIPT_ROOT = Path(__file__).resolve().parent
if str(SCRIPT_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPT_ROOT))

from common import build_error_payload, get_env, print_json


DEFAULT_BASE_URL = "http://resource.seewo.com/teaching-datasets"
TOPIC_SUFFIX_PATTERNS = (
    "教学设计",
    "课程标准",
    "教学目标",
    "教学方法",
    "课堂活动",
    "教学案例",
    "教学重点",
    "教学难点",
    "教学重难点",
    "教材解析",
    "文本解读",
    "课文解析",
    "课文内容",
    "阅读教学",
    "主题思想",
    "情感教育",
    "课件",
)
METADATA_TOKENS = (
    "小学",
    "初中",
    "高中",
    "一年级",
    "二年级",
    "三年级",
    "四年级",
    "五年级",
    "六年级",
    "七年级",
    "八年级",
    "九年级",
    "高一",
    "高二",
    "高三",
    "语文",
    "数学",
    "英语",
    "物理",
    "化学",
    "生物",
    "历史",
    "地理",
    "政治",
    "道德与法治",
)


def resolve_base_url() -> str:
    return (
        get_env("LESSON_PLAN_KB_SEARCH_BASE_URL")
        or get_env("KNOWLEDGE_BASE_SEARCH_BASE_URL")
        or DEFAULT_BASE_URL
    )


def normalize_keyword(keyword: str) -> str:
    normalized = re.sub(r"\s+", " ", keyword.strip())
    if not normalized:
        return normalized

    title_match = re.search(r"《([^》]+)》", normalized)
    if title_match:
        return title_match.group(1).strip()

    for suffix in TOPIC_SUFFIX_PATTERNS:
        if suffix in normalized:
            normalized = normalized.split(suffix, 1)[0].strip()
            break

    if " " in normalized:
        tokens = [token.strip() for token in normalized.split(" ") if token.strip()]
        content_tokens = [
            token for token in tokens if not any(marker in token for marker in METADATA_TOKENS)
        ]
        if content_tokens:
            return content_tokens[0]
        return tokens[0]

    return normalized


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Search teaching knowledge base content.")
    parser.add_argument("--keyword", required=True)
    parser.add_argument("--x-token", default="", help=argparse.SUPPRESS)
    parser.add_argument(
        "--base-url",
        default=resolve_base_url(),
    )
    return parser.parse_args(argv)


def search_knowledge_base(*, base_url: str, keyword: str) -> dict[str, str]:
    query_string = parse.urlencode({"keyWord": keyword})
    url = f"{base_url.rstrip('/')}/api/v1/teaching/datasets/search-with-origin-content?{query_string}"
    req = request.Request(url, method="GET")

    with request.urlopen(req, timeout=30) as response:
        data = json.loads(response.read().decode("utf-8"))

    if data.get("code") != 0:
        return {
            "chapterName": "",
            "content": "",
            "errorMsg": data.get("errorMsg", ""),
        }

    result_data = data.get("data", {})
    return {
        "chapterName": result_data.get("chapterName", ""),
        "content": result_data.get("teachingAllResourceContentStr", ""),
    }


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    normalized_keyword = normalize_keyword(args.keyword)
    try:
        result = search_knowledge_base(base_url=args.base_url, keyword=normalized_keyword)
    except error.HTTPError as exc:
        print_json(
            build_error_payload(
                f"knowledge base search http error: {exc.code}",
                provider="knowledge_base_search",
                keyword=normalized_keyword,
                original_keyword=args.keyword,
            )
        )
        return 1
    except Exception as exc:
        print_json(
            build_error_payload(
                f"knowledge base search failed: {exc}",
                provider="knowledge_base_search",
                keyword=normalized_keyword,
                original_keyword=args.keyword,
            )
        )
        return 1

    print_json(
        {
            "success": True,
            "provider": "knowledge_base_search",
            "keyword": normalized_keyword,
            "original_keyword": args.keyword,
            "result": result,
        }
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
