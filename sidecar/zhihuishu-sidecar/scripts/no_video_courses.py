"""「这门课没有视频资源」的记忆: 纯函数, 单独成模块好测.

为什么需要(2026-11-22):
polymas 的「26-27-1思想道德与法治」(27 个资源) 与「职业生涯规划」(7 个资源),
资源全是 PDF/PPT/DOCX, **一个视频都没有**。但目标生成器只按「有内容 + 没刷完」挑课,
于是这两门课每晚都进清单, 刷课器要进课、翻一遍资源树、再判定「没有待刷的视频」
走人。更糟的是它们占满了清单名额(max=3), 把真有待刷视频的「国家安全教育」
(progress=60) 挤到第 4 位 → 2026-11-22 那一晚根本没刷它。

所以刷课器探到「这门课没有视频」就记一笔, 目标生成器据此排除。

带时效(默认 14 天): 老师以后补了视频, 到期自动重新探测 —— 不排除成永久的。
"""
from __future__ import annotations

import json
import os

# 「没有视频」这个判定的有效期(天)。过期后重新探测, 防止老师补了视频却不刷。
DEFAULT_TTL_DAYS = 14


def load_store(path: str) -> dict[str, dict]:
    """读记忆文件; 不存在或坏掉都当空(别让一个脏文件把刷课链路带崩)。"""
    try:
        with open(path, encoding="utf-8") as fh:
            data = json.load(fh)
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def save_store(path: str, store: dict[str, dict]) -> None:
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(store, fh, ensure_ascii=False, indent=2)
    os.replace(tmp, path)


def mark(store: dict[str, dict], url: str, title: str, now_ts: int) -> dict[str, dict]:
    """记一笔「这门课没有视频」。"""
    store[url] = {"title": title, "checked_at": int(now_ts)}
    return store


def is_excluded(
    store: dict[str, dict], url: str, now_ts: int, ttl_days: int = DEFAULT_TTL_DAYS
) -> bool:
    """这门课现在该不该被目标生成器排除。"""
    entry = store.get(url)
    if not isinstance(entry, dict):
        return False
    checked_at = entry.get("checked_at")
    if not isinstance(checked_at, int):
        return False
    age_days = (int(now_ts) - checked_at) / 86400
    return age_days < ttl_days


def prune(store: dict[str, dict], now_ts: int, ttl_days: int = DEFAULT_TTL_DAYS) -> dict[str, dict]:
    """清掉过期条目, 返回清理后的 store(就地改, 也返回同一份便于链式)。"""
    for url in [
        url
        for url, entry in store.items()
        if not isinstance(entry, dict)
        or not isinstance(entry.get("checked_at"), int)
        or (int(now_ts) - entry["checked_at"]) / 86400 >= ttl_days
    ]:
        store.pop(url, None)
    return store


def excluded_urls(
    path: str, now_ts: int, ttl_days: int = DEFAULT_TTL_DAYS
) -> set[str]:
    """记忆文件里「现在仍然排除」的课程 URL 集合。"""
    store = load_store(path)
    return {
        url
        for url in store
        if is_excluded(store, url, now_ts, ttl_days)
    }
