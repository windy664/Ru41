#!/usr/bin/env bash
# 取 jwxt 插件当前的 CAS TGT（运动世界校园登录要借它）。
#
# 为什么单独成文件：systemd 的 ExecStart 会把 \" 吃掉 → python 代码里的引号没了 →
# 「SyntaxError: invalid syntax」（2026-11-23 实测：学习平台那轮单元直接失败）。
# 放进脚本就没这层转义地狱。
set -euo pipefail

docker exec ru41-jwxt python3 -c '
import json
with open("/home/node/.ru41/jwxt/session.json") as fh:
    print(json.load(fh).get("cas_tgt", ""))
'
