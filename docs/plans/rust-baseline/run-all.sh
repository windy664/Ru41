#!/bin/sh
# 编译并运行全部学习块示例。
# 用法：sh docs/plans/rust-baseline/run-all.sh
# 需要 rustc >= 1.85（支持 edition 2024）。
set -e
cd "$(dirname "$0")"
fail=0
for f in block*.rs; do
  b="${f%.rs}"
  printf '=== %s ===\n' "$b"
  if rustc --edition 2024 --test "$f" -o "/tmp/$b" 2>"/tmp/$b.err"; then
    "/tmp/$b" 2>&1 | tail -2
  else
    fail=1
    echo '编译失败:'; head -20 "/tmp/$b.err"
  fi
done
[ "$fail" -eq 0 ] && echo && echo '全部通过' || { echo; echo '有失败'; exit 1; }
