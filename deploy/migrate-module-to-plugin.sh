#!/usr/bin/env bash
# 模块迁插件（机械部分）：git mv + 引路改写 + 入口函数 + Cargo.toml。
# 用法：./deploy/migrate-module-to-plugin.sh <module-name>
# 之后人工：编译修错、poll_json 过桥、测试搬迁。
set -euo pipefail
cd "$(dirname "$0")/.."
M="$1"
SRC="src/modules/$M"
DST="plugins/$M"
[ -d "$SRC" ] || { echo "no such module: $SRC"; exit 1; }
[ ! -e "$DST" ] || { echo "target exists: $DST"; exit 1; }
mkdir -p "$DST"

git mv "$SRC" "$DST/src"
git mv "$DST/src/mod.rs" "$DST/src/lib.rs"

python3 - "$M" <<'PYEOF'
import pathlib, re, sys

m = sys.argv[1]
dst = pathlib.Path(f'plugins/{m}/src')
lib = dst / 'lib.rs'
for p in dst.rglob('*.rs'):
    s = p.read_text()
    # 自引用先换（crate::modules::<m>:: → crate::），再整体 crate:: → ru41::
    s = s.replace(f'crate::modules::{m}::', 'crate::')
    s = s.replace(f'crate::modules::{m};', 'crate::')
    s = re.sub(rf'crate::modules::{m}\b', 'crate', s)
    s = s.replace('crate::', 'ru41::')
    # modules 父级引用 → ru41::modules::
    s = s.replace('super::ModuleDef', 'ru41::modules::ModuleDef')
    s = s.replace('super::JobSchedule', 'ru41::modules::JobSchedule')
    s = s.replace('super::DEFAULT_PRIORITY', 'ru41::modules::DEFAULT_PRIORITY')
    # crate 根（旧 mod.rs）里的 super::no_* → ru41::modules::no_*
    s = s.replace('super::no_cli', 'ru41::modules::no_cli')
    s = s.replace('super::no_image_consumers', 'ru41::modules::no_image_consumers')
    s = s.replace('use super::', 'use crate::')
    # 去掉 linkme 自注册属性（插件走入口函数）
    s = s.replace('#[linkme::distributed_slice(ru41::modules::MODULES)]\n', '')
    s = s.replace('#[linkme::distributed_slice(super::MODULES)]\n', '')
    # 子模块测试里的 use super::* 指本模块自身，不能跟着改成 use crate::*
    if p != lib:
        s = s.replace('use crate::*;', 'use super::*;')
    p.write_text(s)

# lib.rs（crate 根）里被误改的自引用：按本文件 pub use/模块声明还原 crate::
s = lib.read_text()
local_names = set(re.findall(r'pub mod (\w+)', s))
for use_stmt in re.findall(r'pub use ([^;]+);', s):
    brace = re.search(r'\{([^}]*)\}', use_stmt)
    if brace:
        local_names |= {n.strip() for n in brace.group(1).split(',') if n.strip()}
    else:
        tail = use_stmt.strip().rsplit('::', 1)[-1].strip()
        if re.fullmatch(r'\w+', tail):
            local_names.add(tail)
for name in sorted(local_names, key=len, reverse=True):
    s = re.sub(rf'ru41::{name}\b', f'crate::{name}', s)
lib.write_text(s)

# 全量文件的模块级自引用还原（teachermate 试点发现）：
# - 顶层模块（src/config.rs、src/service/ 等）在任何文件里都应指回 crate::
# - 子目录文件里的兄弟模块引用（source/client.rs 的 super::model）被
#   use super:: → use crate:: 误升格，按子目录成员表还原回 super::
top_names = {p.stem for p in dst.glob('*.rs') if p.stem != 'lib'}
top_names |= {p.name for p in dst.iterdir() if p.is_dir()}
for p in dst.rglob('*.rs'):
    if p == lib:
        continue
    s = p.read_text()
    for name in sorted(top_names, key=len, reverse=True):
        s = re.sub(rf'ru41::{name}\b', f'crate::{name}', s)
    if p.parent != dst:
        siblings = {q.stem for q in p.parent.glob('*.rs') if q.stem not in ('mod', p.stem)}
        siblings |= {q.name for q in p.parent.iterdir() if q.is_dir()}
        for name in sorted(siblings, key=len, reverse=True):
            s = re.sub(rf'use crate::{name}\b', f'use super::{name}', s)
    p.write_text(s)

# 入口函数附加到 lib.rs
lib = dst / 'lib.rs'
s = lib.read_text()
if 'ru41_plugin_entry' not in s:
    s += f'''
/// 宿主加载入口（插件不用 linkme，走显式注册）。
/// feature 门控：被其他插件依赖时不导出，编 .so 必须 --features plugin-entry。
#[cfg(feature = "plugin-entry")]
#[allow(improper_ctypes_definitions)]
#[unsafe(no_mangle)]
pub extern "C" fn ru41_plugin_entry() -> ru41::plugins::PluginEntry {{
    ru41::plugins::PluginEntry::new("{m}", || DEF)
}}
'''
    lib.write_text(s)

# modules/mod.rs 删注册行
mm = pathlib.Path('src/modules/mod.rs')
s = mm.read_text()
s = s.replace(f'pub mod {m};\n', '')
mm.write_text(s)
print(f'{m}: moved')
PYEOF

cat > "$DST/Cargo.toml" <<EOF
[package]
name = "ru41-plugin-$M"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib", "rlib"]

[features]
plugin-entry = []

[dependencies]
# 必须与宿主同一版本（ABI 握手校验 git commit + pkg 版本）。
ru41 = { path = "../.." }
anyhow = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.9"
chrono = { version = "0.4", default-features = false, features = ["std", "clock"] }
regex = "1"
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "cookies", "blocking"] }
tokio = { version = "1", features = ["full"] }
futures-util = "0.3"
EOF
echo "$M: Cargo.toml written"
