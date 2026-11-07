# Downloads ONNX Runtime with DirectML and puts its DLLs in runtime/.
import io, json, pathlib, urllib.request, zipfile

VERSION = "1.24.4"
out = pathlib.Path(__file__).resolve().parent.parent / "runtime"
out.mkdir(exist_ok=True)
meta = json.load(urllib.request.urlopen(f"https://pypi.org/pypi/onnxruntime-directml/{VERSION}/json"))
wheel = next(u["url"] for u in meta["urls"] if "win_amd64" in u["filename"])
z = zipfile.ZipFile(io.BytesIO(urllib.request.urlopen(wheel).read()))
for n in z.namelist():
    if n.endswith((".dll",)) and "/capi/" in n and "providers_shared" not in n:
        (out / pathlib.Path(n).name).write_bytes(z.read(n))
        print("wrote", pathlib.Path(n).name)
