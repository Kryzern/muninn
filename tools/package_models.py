# Prepares the model downloads: copies each exported model into dist/models
# as <id>-<file>, ready to attach to the `models-v1` GitHub release, and
# writes src/models.json, the catalog compiled into the app (sizes + SHA-256).
#
#   python tools/package_models.py
import hashlib, json, pathlib, shutil

root = pathlib.Path(__file__).resolve().parent.parent
out = root / "dist" / "models"
out.mkdir(parents=True, exist_ok=True)

MODELS = [
    dict(id="fast", name="Fast", arch="ViT-B/32", dim=512, src=root / "models",
         blurb="Quick to set up and quick on any PC, integrated graphics included. Good for most libraries."),
    dict(id="balanced", name="Balanced", arch="ViT-B/16", dim=512, src=root / "models-out" / "b16",
         blurb="Noticeably better at small details and busy scenes. Reading your photos takes about three times longer."),
    dict(id="best", name="Best", arch="ViT-L/14", dim=768, src=root / "models-out" / "l14",
         blurb="The most accurate. Wants a dedicated graphics card, and reading a large library takes a while."),
]
FILES = [("image-f16.onnx", False), ("text-f16.onnx", False), ("tokenizer.json", False), ("image-f32.onnx", True), ("text-f32.onnx", True)]


def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


catalog = []
for m in MODELS:
    files = []
    for name, cpu in FILES:
        src = m["src"] / name
        dst = out / f"{m['id']}-{name}"
        if not dst.exists() or dst.stat().st_size != src.stat().st_size:
            shutil.copy(src, dst)
        files.append(dict(name=name, size=src.stat().st_size, sha256=sha(src), cpu=cpu))
        print(dst.name, round(src.stat().st_size / 1e6), "MB")
    catalog.append(dict(id=m["id"], name=m["name"], arch=m["arch"], dim=m["dim"], blurb=m["blurb"], files=files))
(root / "src" / "models.json").write_text(json.dumps(catalog, indent=1))
print("wrote src/models.json")
