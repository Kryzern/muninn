# Exports an OpenCLIP model to ONNX: fp16 for DirectML, and fp32 for the CPU.
#
#   python tools/export_onnx.py                          # ViT-B/32 into models/ (the dev tree)
#   python tools/export_onnx.py ViT-B-16 out/b16         # any OpenCLIP architecture, DataComp-XL weights
#   python tools/export_onnx.py ViT-L-14 out/l14 --pretrained datacomp_xl_s13b_b90k
import argparse, pathlib, shutil
import torch, open_clip, onnx
from onnxruntime.transformers.float16 import convert_float_to_float16

root = pathlib.Path(__file__).resolve().parent.parent
ap = argparse.ArgumentParser()
ap.add_argument("arch", nargs="?", default="ViT-B-32")
ap.add_argument("out", nargs="?", default=str(root / "models"))
ap.add_argument("--pretrained", default="datacomp_xl_s13b_b90k")
ap.add_argument("--no-cpu", action="store_true", help="skip the fp32 CPU files")
a = ap.parse_args()
out = pathlib.Path(a.out)
out.mkdir(parents=True, exist_ok=True)

model, _, _ = open_clip.create_model_and_transforms(a.arch, pretrained=a.pretrained)
model.eval()
torch.backends.mha.set_fastpath_enabled(False)  # the fused kernel has no ONNX export


class Image(torch.nn.Module):
    def __init__(self, m): super().__init__(); self.m = m
    def forward(self, pixels): return self.m.encode_image(pixels, normalize=True)


class Text(torch.nn.Module):
    def __init__(self, m): super().__init__(); self.m = m
    def forward(self, ids): return self.m.encode_text(ids, normalize=True)


def export(mod, arg, name, inp):
    f32 = out / f"{name}-f32.onnx"
    torch.onnx.export(mod, arg, f32, input_names=[inp], output_names=["embeds"],
                      dynamic_axes={inp: {0: "n"}, "embeds": {0: "n"}}, opset_version=17, dynamo=False)
    m = onnx.load(f32)
    onnx.save(convert_float_to_float16(m, keep_io_types=True), out / f"{name}-f16.onnx")
    if a.no_cpu:
        f32.unlink()


with torch.no_grad():
    export(Image(model), torch.zeros(1, 3, 224, 224), "image", "pixels")
    export(Text(model), torch.zeros(1, 77, dtype=torch.long), "text", "ids")
tok = root / "models" / "tokenizer.json"
if tok.exists() and out.resolve() != tok.parent.resolve():
    shutil.copy(tok, out / "tokenizer.json")
for f in sorted(out.iterdir()):
    print(f.name, round(f.stat().st_size / 1e6, 1), "MB")
