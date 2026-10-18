"""生成 README 原创流程插图；依赖 Pillow，不读取或修改外部图片。"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent
FONT = '/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf'
W, H = 1200, 370
BG, PANEL, LINE = '#101923', '#172432', '#32495b'
TEXT, MUTED, ACCENT = '#e6edf3', '#9eb2c4', '#62d8bd'

def font(size):
    return ImageFont.truetype(FONT, size)

def frame(t):
    im = Image.new('RGB', (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((42, 24), 'Ru41 / PERSONAL AUTOMATION', font=font(25), fill=TEXT)
    d.text((42, 65), 'Your inputs. A Rust host. Replaceable agents.', font=font(15), fill=MUTED)
    nodes = [(42, 'INPUT', ['CLI / HTTP / QQ', 'arXiv / RSS']),
             (330, 'RUST HOST', ['Queue + Worker', 'Privacy boundary']),
             (618, 'AGENT', ['Codex / Pi', 'analysis_v1']),
             (906, 'RESULT', ['Validate + save', 'QQ / Feishu'])]
    for x, title, lines in nodes:
        d.rounded_rectangle((x, 124, x+252, 258), radius=14, fill=PANEL, outline=LINE, width=2)
        d.text((x+18, 143), title, font=font(17), fill=ACCENT)
        for j, line in enumerate(lines):
            d.text((x+18, 184+j*25), line, font=font(14), fill=TEXT if j==0 else MUTED)
    for start in (294, 582, 870):
        end = start+36
        d.line((start, 191, end, 191), fill=LINE, width=2)
        d.polygon([(end-6,187),(end,191),(end-6,195)],fill=MUTED)
        x = start+4+(end-start-10)*t
        d.ellipse((x-3,188,x+3,194), fill=ACCENT)
    d.line((42, 289, 1158, 289), fill=LINE)
    d.text((42, 311), 'SQLite / PostgreSQL', font=font(14), fill=MUTED)
    d.text((438, 311), 'Durable events + SSE', font=font(14), fill=MUTED)
    d.text((906, 311), 'Modular by design', font=font(14), fill=MUTED)
    return im

if __name__ == '__main__':
    frames = [frame(i/31) for i in range(32)]
    frames[16].save(ROOT/'task-flow.png', optimize=True)
    frames[0].save(ROOT/'task-flow.gif', save_all=True, append_images=frames[1:],
                   duration=80, loop=0, optimize=True, disposal=1)
    print('Generated task-flow.gif and task-flow.png')
