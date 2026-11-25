"""从 SVG 重新导出 PNG 与 ICO：python export.py

用无界面 Edge 按每个尺寸原生渲染（不是把大图缩小），再裁切为 png/ 下的各尺寸文件，
并用 16–256px 的原生渲染组成 app-icon.ico。依赖：Microsoft Edge、Pillow。
"""
import subprocess
import tempfile
from pathlib import Path
from PIL import Image, ImageChops

HERE = Path(__file__).resolve().parent
EDGE = Path(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")
SMALL = [16, 20, 24, 32, 40, 48, 64, 128, 256, 512]
ICO = [16, 20, 24, 32, 40, 48, 64, 256]


def render(work: Path) -> Image.Image:
    base = HERE.as_uri()
    imgs, x = [], 0
    for size in SMALL:
        imgs.append(f"<img src='{base}/app-icon.svg' style='left:{x}px;top:1040px;width:{size}px;height:{size}px'>")
        x += size + 10
    page = work / "render.html"
    page.write_text(
        "<!DOCTYPE html><html><head><style>html,body{margin:0;background:transparent}img{position:absolute}</style>"
        f"</head><body><img src='{base}/app-icon.svg' style='left:0;top:0;width:1024px;height:1024px'>"
        f"<img src='{base}/mark-color.svg' style='left:1040px;top:0;width:1024px;height:1024px'>{''.join(imgs)}</body></html>",
        encoding="utf-8")
    shot = work / "render.png"
    subprocess.run([str(EDGE), "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                    "--default-background-color=00000000", "--allow-file-access-from-files", "--window-size=2080,1580",
                    f"--user-data-dir={work / 'profile'}", f"--screenshot={shot}", page.as_uri()], check=True)
    return Image.open(shot).convert("RGBA")


def main():
    with tempfile.TemporaryDirectory() as tmp:
        sheet = render(Path(tmp))
    (HERE / "png").mkdir(exist_ok=True)
    icons = {1024: sheet.crop((0, 0, 1024, 1024))}
    x = 0
    for size in SMALL:
        icons[size] = sheet.crop((x, 1040, x + size, 1040 + size))
        x += size + 10
    for size, img in icons.items():
        img.save(HERE / "png" / f"app-icon-{size}.png")
    sheet.crop((1040, 0, 2064, 1024)).save(HERE / "png" / "mark-color-1024.png")
    icons[256].save(HERE / "app-icon.ico", format="ICO", sizes=[(s, s) for s in ICO],
                    append_images=[icons[s] for s in ICO if s != 256])

    # 校验：ICO 每帧都是对应尺寸的原生渲染；四角透明（圆角抗锯齿允许极小 alpha）、中心不透明
    ico = Image.open(HERE / "app-icon.ico")
    assert sorted(ico.info["sizes"]) == [(s, s) for s in ICO], ico.info["sizes"]
    for s in ICO:
        ico.size = (s, s)
        assert ImageChops.difference(ico.convert("RGBA"), icons[s]).getbbox() is None, f"{s}px frame mismatch"
    for size, img in icons.items():
        assert img.getpixel((0, 0))[3] <= 8 and img.getpixel((size // 2, size // 2))[3] == 255, size
    print("ok:", ", ".join(f"{s}px" for s in sorted(icons)), "| ico:", ICO)


if __name__ == "__main__":
    main()
