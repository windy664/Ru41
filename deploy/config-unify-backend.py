#!/usr/bin/env python3
"""底座统一到 pi + 后台来源静默：改生产机的 ru41.toml / ru41-worker.toml。

做三件事：
1. 写入统一入口 [agent] default_backend = "pi"（放在 [agent.*] 子表之前）。
2. 删掉 [agent.codex] 与 [agent.codex.provider]（GLM 供应方下线）。
3. 在 [notify] 段加 silent_sources：后台服务类来源不再推送 agent 过程。

幂等：已改过的文件再跑一次不会重复插入。
"""
import re
import shutil
import sys
import time

SILENT_SOURCES = [
    "obsidian",
    "papers",
    "research",
    "programs",
    "projects",
    "competitions",
]


def drop_table(text: str, header: str) -> str:
    """删掉某个 TOML 表段（含其子表）。"""
    pattern = re.compile(
        r"^\[" + re.escape(header) + r"\][^\[]*?(?=^\[|\Z)", re.M | re.S
    )
    return pattern.sub("", text)


def ensure_default_backend(text: str) -> str:
    if re.search(r"^default_backend\s*=", text, re.M):
        text = re.sub(
            r"^default_backend\s*=.*$",
            'default_backend = "pi"',
            text,
            count=1,
            flags=re.M,
        )
        return text
    # 插到第一个 [agent.*] 子表之前
    match = re.search(r"^\[agent(\.[^\]]+)?\]", text, re.M)
    block = '[agent]\ndefault_backend = "pi"\n'
    if match:
        return text[: match.start()] + block + "\n" + text[match.start() :]
    return text.rstrip("\n") + "\n\n" + block


PI_BASE_URL = "https://api.minimax.io/anthropic"
PI_API = "anthropic-messages"


def ensure_pi_provider(text: str) -> str:
    """把底座供应商的端点与协议写明——供应商只在一处定义（AI 合并也复用它）。"""
    match = re.search(r"^\[agent\.pi\][^\[]*?(?=^\[|\Z)", text, re.M | re.S)
    if not match:
        return text
    block = match.group(0)
    new_block = block
    if not re.search(r"^base_url\s*=", new_block, re.M):
        new_block = new_block.rstrip("\n") + f"\nbase_url = \"{PI_BASE_URL}\""
    if not re.search(r"^api\s*=", new_block, re.M):
        new_block = new_block.rstrip("\n") + f"\napi = \"{PI_API}\""
    if not new_block.endswith("\n"):
        new_block += "\n"
    return text[: match.start()] + new_block + text[match.end() :]


def ensure_silent_sources(text: str) -> str:
    if re.search(r"^silent_sources\s*=", text, re.M):
        return re.sub(
            r"^silent_sources\s*=.*$",
            "silent_sources = [" + ", ".join(f'"{s}"' for s in SILENT_SOURCES) + "]",
            text,
            count=1,
            flags=re.M,
        )
    match = re.search(r"^\[notify\][^\[]*?(?=^\[|\Z)", text, re.M | re.S)
    if not match:
        raise SystemExit("找不到 [notify] 段")
    block = match.group(0)
    new_block = block.rstrip("\n") + "\nsilent_sources = [" + ", ".join(
        f'"{s}"' for s in SILENT_SOURCES
    ) + "]\n"
    return text[: match.start()] + new_block + text[match.end() :]


def main(paths: list[str]) -> int:
    for path in paths:
        original = open(path, encoding="utf-8").read()
        text = original
        text = drop_table(text, "agent.codex.provider")
        text = drop_table(text, "agent.codex")
        text = ensure_default_backend(text)
        text = ensure_pi_provider(text)
        text = ensure_silent_sources(text)
        if text == original:
            print(f"SKIP {path}: 已经是目标状态")
            continue
        backup = f"{path}.bak-{time.strftime('%Y%m%d-%H%M%S')}"
        shutil.copy2(path, backup)
        open(path, "w", encoding="utf-8").write(text)
        print(f"OK   {path}（备份 {backup}）")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
