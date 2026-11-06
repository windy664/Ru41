#!/usr/bin/env bash
# Vault 瘦身并强推：清掉历史里的大文件（学生手册 PDF、竞赛二进制），
# 让 gitee 仓库从 1GB 降到 ~35MB。
#
# 前置：仓库已超配额时，gitee 的 pre-receive 会拒掉 force push
#      （社区已知「配额死锁」），必须先在网页上点一次「存储库 GC」。
#
# 用法：./deploy/vault-slim-and-push.sh <服务器上 vault 路径> [远端分支]
set -euo pipefail

VAULT_PATH="${1:-/vault}"
BRANCH="${2:-master}"
# git-filter-repo 放哪都行：优先常见的几个位置。
FILTER=""
for candidate in ~/bin/git-filter-repo ~/.local/bin/git-filter-repo /home/node/bin/git-filter-repo "$(command -v git-filter-repo 2>/dev/null || true)"; do
  [ -n "$candidate" ] && [ -x "$candidate" ] && { FILTER="$candidate"; break; }
done
if [ -z "$FILTER" ]; then
  echo "找不到 git-filter-repo：curl -o ~/bin/git-filter-repo https://raw.githubusercontent.com/newren/git-filter-repo/master/git-filter-repo && chmod +x ~/bin/git-filter-repo" >&2
  exit 1
fi

cd "$VAULT_PATH"

echo "==> 瘦身前"
du -sh .git
git ls-files | wc -l | sed 's/^/文件数: /'

# 这些路径在当前工作区早已不存在，只剩历史里的副本：
#   - 学生手册 PDF（已完整转成 md）
#   - Tsec-Hackathon 竞赛 PPT/PDF（08-15 一次性克隆进 vault 的残留）
"$FILTER" --force \
  --path "03-Knowledge-知识/School-学校/学生手册（2026年9月版）.pdf" --invert-paths \
  --path "05-Opportunities-机会/01-Competitions-竞赛/Cybersecurity-网络安全/Tsec-Hackathon" --invert-paths \
  --path "05-Opportunities-机会/Competitions/Tsec-Hackathon" --invert-paths

echo "==> 瘦身后"
du -sh .git
git ls-files | wc -l | sed 's/^/文件数: /'

echo "==> 强推 $BRANCH"
# filter-repo 会把 origin 删掉，这里按原来的地址补回来。
REMOTE_URL="$(git config --get remote.origin.url || echo https://gitee.com/sui-heng/obsidian-vault.git)"
git remote get-url origin >/dev/null 2>&1 || git remote add origin "$REMOTE_URL"
git push --force origin "HEAD:refs/heads/$BRANCH"

echo "==> 清理本地旧对象"
git reflog expire --expire=now --all
git gc --prune=now --quiet
du -sh .git
echo "==> 完成：远端再点一次 gitee 的「存储库 GC」即可把旧 master 的对象也回收掉"
