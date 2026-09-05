#!/usr/bin/env python3
"""ON/OFF のタスクトレイアイコンを生成する。

生成物（src/icons/ に出力）:
  - icon_on.ico / icon_on.png    … 黒「B」
  - icon_off.ico / icon_off.png  … 黒「B」＋赤い禁止マーク
  - app.ico                      … 実行ファイル自身のアイコン（ON と同じ）
  - icon_on@256.png / icon_off@256.png … README / プレビュー用の大きめ PNG

依存: Pillow (`pip install Pillow`)
使い方: リポジトリ直下で `python3 tools/gen_icons.py`
"""

import math
import os

from PIL import Image, ImageDraw, ImageFont

# 太字フォント（環境に合わせて変更可）。
FONT_CANDIDATES = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
    "/Library/Fonts/Arial Bold.ttf",
    "C:/Windows/Fonts/arialbd.ttf",
]

# スーパーサンプリング用のマスターサイズ。
MASTER = 256
# ICO / PNG に含めるサイズ。
SIZES = [16, 24, 32, 48, 64, 128, 256]

HERE = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.normpath(os.path.join(HERE, "..", "src", "icons"))


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if os.path.exists(path):
            return ImageFont.truetype(path, size)
    raise SystemExit("太字 TTF フォントが見つかりません。FONT_CANDIDATES を編集してください。")


def draw_b(img: Image.Image, stroke_width: int = 0, stroke_fill=None) -> None:
    d = ImageDraw.Draw(img)
    font = load_font(210)
    bbox = d.textbbox((0, 0), "B", font=font, stroke_width=stroke_width)
    w, h = bbox[2] - bbox[0], bbox[3] - bbox[1]
    x = (MASTER - w) / 2 - bbox[0]
    y = (MASTER - h) / 2 - bbox[1]
    # 文字色は黒のまま。stroke_* を渡すと縁取りを付ける。
    d.text(
        (x, y),
        "B",
        font=font,
        fill=(15, 15, 15, 255),
        stroke_width=stroke_width,
        stroke_fill=stroke_fill,
    )


def draw_white_disc(img: Image.Image) -> None:
    """禁止マークの内側を埋める白い円（背景）を描く。"""
    d = ImageDraw.Draw(img)
    margin = 14  # draw_prohibition の円と同じ位置に合わせる
    d.ellipse([margin, margin, MASTER - margin, MASTER - margin], fill=(255, 255, 255, 255))


def draw_prohibition(img: Image.Image) -> None:
    d = ImageDraw.Draw(img)
    red = (237, 28, 36, 255)
    margin, ring = 14, 22
    d.ellipse([margin, margin, MASTER - margin, MASTER - margin], outline=red, width=ring)
    cx = cy = MASTER / 2
    r = (MASTER - 2 * margin) / 2 - ring / 2
    a = math.radians(45)
    dx, dy = math.cos(a) * r, math.sin(a) * r
    d.line([cx - dx, cy - dy, cx + dx, cy + dy], fill=red, width=ring)


def save_set(img: Image.Image, name: str) -> None:
    # 256 のマスターから保存する。ICO は必ず「大きい画像」を基準に保存すること。
    # 16px 画像を基準にすると Pillow は 16px しか内包せず、拡大表示でぼやける。
    master = img if img.size == (256, 256) else img.resize((256, 256), Image.LANCZOS)
    # プレビュー用 PNG（16px と 256px）
    master.resize((16, 16), Image.LANCZOS).save(os.path.join(OUT_DIR, f"{name}.png"))
    master.save(os.path.join(OUT_DIR, f"{name}@256.png"))
    # マルチサイズ ICO（16〜256 を内包。Pillow が各サイズを LANCZOS で生成）
    master.save(
        os.path.join(OUT_DIR, f"{name}.ico"),
        format="ICO",
        sizes=[(s, s) for s in SIZES],
    )


def main() -> None:
    os.makedirs(OUT_DIR, exist_ok=True)

    # ON: 黒い「B」＋白い縁取り（暗い背景でも見やすく）。
    # 縁取りが細いと 16px 縮小時に 1px 未満になり、既定が黒い
    # Windows 11 のタスクバーで「B」が背景に溶けて見えなくなるため、
    # 暗い背景でも視認できる太さ（かつ「B」の穴を潰さない範囲）に調整している。
    on = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    draw_b(on, stroke_width=20, stroke_fill=(255, 255, 255, 255))
    save_set(on, "icon_on")

    # OFF: 禁止マーク内を白背景にし、その上に黒い「B」＋赤い禁止マーク。
    off = Image.new("RGBA", (MASTER, MASTER), (0, 0, 0, 0))
    draw_white_disc(off)
    draw_b(off)
    draw_prohibition(off)
    save_set(off, "icon_off")

    # 実行ファイルアイコンは OFF と同じ絵柄（禁止マーク付き・白背景・マルチサイズ ICO）。
    off.save(
        os.path.join(OUT_DIR, "app.ico"),
        format="ICO",
        sizes=[(s, s) for s in SIZES],
    )

    print(f"アイコンを生成しました: {OUT_DIR}")


if __name__ == "__main__":
    main()
