"""生成项目封面：原创文字与几何图形，依赖 Pillow 和系统 Noto 字体。"""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont
import math
W,H=1600,600
im=Image.new('RGB',(W,H),'#101923'); d=ImageDraw.Draw(im)
regular='/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc'
bold='/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf'
def label(x,y,s,size,color='#e6edf3',face=regular):
 d.text((x,y),s,font=ImageFont.truetype(face,size),fill=color)
for x in range(32,W,40):
 for y in range(30,H,40): d.ellipse((x,y,x+1,y+1),fill='#243442')
d.rounded_rectangle((44,44,W-44,H-44),radius=26,outline='#334956',width=2)
label(96,88,'PERSONAL AUTOMATION / RUST HOST',20,'#62d8bd')
label(86,142,'Ru41',132,face=bold)
label(96,318,'让信息有去处，让想法有后续。',34)
label(96,385,'可替换 Agent  ·  模块化业务  ·  自己掌控数据',22,'#a7bdcd')
d.line((96,474,750,474),fill='#334956',width=2)
label(96,491,'INGEST   /   ANALYZE   /   DELIVER',17,'#a7bdcd')
cx,cy=1180,300
for r in (125,195): d.ellipse((cx-r,cy-r,cx+r,cy+r),outline='#334956',width=2)
for angle,name in [(-90,'INPUT'),(30,'AGENT'),(150,'OUTPUT')]:
 a=math.radians(angle);x=cx+195*math.cos(a);y=cy+195*math.sin(a)
 d.line((cx,cy,x,y),fill='#466275',width=3)
 d.rounded_rectangle((x-61,y-24,x+61,y+24),radius=12,fill='#1b2d3a',outline='#62d8bd',width=2)
 f=ImageFont.truetype(bold,15);d.text((x,y),name,font=f,fill='#dce8ef',anchor='mm')
d.rounded_rectangle((cx-86,cy-65,cx+86,cy+65),radius=22,fill='#203c43',outline='#62d8bd',width=3)
d.text((cx,cy-12),'Ru41',font=ImageFont.truetype(bold,38),fill='#e6edf3',anchor='mm')
d.text((cx,cy+30),'RUST CORE',font=ImageFont.truetype(bold,13),fill='#62d8bd',anchor='mm')
im.save(Path(__file__).resolve().parent/'cover.png',optimize=True)
