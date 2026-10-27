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
    s = s.replace('use super::', 'use crate::')
    # 去掉 linkme 自注册属性（插件走入口函数）
    s = s.replace('#[linkme::distributed_slice(ru41::modules::MODULES)]\n', '')
    s = s.replace('#[linkme::distributed_slice(super::MODULES)]\n', '')
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
