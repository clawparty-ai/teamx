#!/usr/bin/env python3
# Build teamx-FDE-pitch.pptx from the rendered 16:9 PNGs (one slide per page).
from pptx import Presentation
from pptx.util import Inches
from pptx.enum.shapes import MSO_SHAPE
import os, sys

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.join(HERE, "out")
PPTX = os.path.join(HERE, "teamx-FDE-pitch.pptx")

slides = [
    ("01-overview.png", "teamx · FDE 组织与治理底座"),
    ("02-why.png", "为什么 AI FDE 需要 teamx"),
    ("03-internal.png", "对内推行 FDE"),
    ("04-customer.png", "对客户推销 FDE"),
]

prs = Presentation()
prs.slide_width = Inches(13.333)   # 16:9
prs.slide_height = Inches(7.5)
blank = prs.slide_layouts[6]       # blank

for fname, title in slides:
    path = os.path.join(OUT_DIR, fname)
    if not os.path.exists(path):
        print(f"missing {path}")
        sys.exit(1)
    slide = prs.slides.add_slide(blank)
    # full-bleed image (slightly scaled to cover any rounding)
    slide.shapes.add_picture(path, 0, 0, Inches(13.333), Inches(7.5))

prs.save(PPTX)
print("saved", PPTX, "slides:", len(prs.slides.__iter__.__self__._sldIdLst))
