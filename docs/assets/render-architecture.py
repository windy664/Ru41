"""绘制 README 架构图，直接输出 PNG，避免依赖 Markdown 图表渲染器。"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent
im = Image.new('RGB', (1200, 960), '#101923')
d = ImageDraw.Draw(im)
FONT = '/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc'

def text(x, y, value, size=22, color='#e6edf3'):
    d.text((x, y), value, font=ImageFont.truetype(FONT, size), fill=color)

def box(x, y, width, title, subtitle):
    d.rounded_rectangle((x, y, x+width, y+92), radius=14, fill='#172432', outline='#466275', width=2)
    text(x+24, y+12, title, 24, '#62d8bd')
    text(x+24, y+49, subtitle, 18, '#b1c2d0')

def arrow(x, top, bottom):
    d.line((x, top, x, bottom-9), fill='#779fc5', width=3)
    d.polygon([(x-6,bottom-10),(x+6,bottom-10),(x,bottom)],fill='#779fc5')

text(48, 24, 'Ru41 / 核心执行架构', 32)
text(48, 72, '入口与执行分离 · 底座与存储可替换', 20, '#9eb2c4')
rows = [
    ('输入与应用服务', 'HTTP / CLI / QQ / 定时来源 → 公共用例与业务模块'),
    ('持久化任务', 'TaskStore · SQLite / PostgreSQL'),
    ('Worker', '领取任务 · 租约续期 · 取消与重试 · 按底座路由'),
    ('脱敏 → AgentBackend', '随机同类假值替换 → Codex / Pi'),
    ('结果处理', '结构化校验 · 真值回填 · 事务保存'),
]
for i, (title, subtitle) in enumerate(rows):
    y=124+i*128
    box(170,y,860,title,subtitle)
    if i<4: arrow(600,y+92,y+128)
# 两个分支均来自结果保存；既有通知不经过事件消费者。
d.line((600,728,600,762),fill='#779fc5',width=3)
d.line((310,762,890,762),fill='#779fc5',width=3)
arrow(310,762,800);arrow(890,762,800)
box(48,800,524,'通知派发','NotifyDispatcher → QQ / 飞书')
box(628,800,524,'持久化状态事件','SSE / 可靠事件消费者')
text(48,916,'审批由底座触发、宿主转发；当前通过 HTTP API 回复。',18,'#9eb2c4')
im.save(ROOT/'architecture.png', optimize=True)
