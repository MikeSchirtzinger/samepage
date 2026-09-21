#!/bin/sh
set -eu

atlas_dir=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd)
model_dir="$atlas_dir/.local/models"
research_dir="$atlas_dir/.local/research/MobileSAM"
environment_dir="$atlas_dir/.local/mobilesam-export-env"
source_url="https://github.com/ChaoningZhang/MobileSAM.git"
source_commit="f706ad9c4eb7f219c00d9050e46328518ffb65d2"
checkpoint_sha="6dbb90523a35330fedd7f1d3dfc66f995213d81b29a5ca8108dbcdd4e37d6c2f"
encoder_sha="d80de6055095e7ba5551e6c15aaa8981b05dca77d6ae77ee82e51e2746a87e3d"
decoder_sha="a21b65b6e1b75e2c6265b36835747a0ab9169ec1ed725139a78ce90297f95126"
encoder_file="$model_dir/mobilesam-vit-t-encoder.onnx"
decoder_file="$model_dir/mobilesam-vit-t-decoder.onnx"

mkdir -p "$model_dir" "$atlas_dir/.local/research"

verify_file() {
  path="$1"
  expected_sha="$2"
  actual_sha=$(shasum -a 256 "$path" | awk '{print $1}')
  if [ "$actual_sha" != "$expected_sha" ]; then
    printf '%s\n' "asset has the wrong digest: $path" >&2
    exit 1
  fi
  printf '%s\n' "verified $path"
}

if [ -f "$encoder_file" ] && [ -f "$decoder_file" ]; then
  verify_file "$encoder_file" "$encoder_sha"
  verify_file "$decoder_file" "$decoder_sha"
else
  command -v uv >/dev/null 2>&1 || {
    printf '%s\n' "uv is required to create the isolated MobileSAM export environment" >&2
    exit 1
  }
  export_python=${MOBILESAM_EXPORT_PYTHON:-}
  if [ -z "$export_python" ]; then
    if command -v python3.13 >/dev/null 2>&1; then
      export_python=$(command -v python3.13)
    else
      printf '%s\n' "Python 3.13 is required. Set MOBILESAM_EXPORT_PYTHON to an explicit interpreter." >&2
      exit 1
    fi
  fi
  if [ ! -d "$research_dir/.git" ]; then
    git clone "$source_url" "$research_dir"
  fi
  git -C "$research_dir" cat-file -e "$source_commit^{commit}" 2>/dev/null || \
    git -C "$research_dir" fetch --depth 1 origin "$source_commit"
  git -C "$research_dir" checkout --detach "$source_commit"
  if [ -n "$(git -C "$research_dir" status --short)" ]; then
    printf '%s\n' "MobileSAM research checkout is dirty: $research_dir" >&2
    exit 1
  fi
  verify_file "$research_dir/weights/mobile_sam.pt" "$checkpoint_sha"

  uv venv --clear "$environment_dir" --python "$export_python"
  uv pip install --python "$environment_dir/bin/python" \
    "torch==2.13.0" \
    "torchvision==0.28.0" \
    "timm==1.0.15" \
    "onnx==1.22.0" \
    "onnxruntime==1.29.0" \
    "numpy==2.5.2" \
    "Pillow==12.3.0"

  build_dir=$(mktemp -d "$atlas_dir/.local/mobilesam-build.XXXXXX")
  trap 'rm -rf "$build_dir"' EXIT HUP INT TERM
  MOBILE_SAM_SOURCE="$research_dir" MOBILE_SAM_OUTPUT="$build_dir" \
    PYTHONPATH="$research_dir" "$environment_dir/bin/python" - <<'PY'
from os import environ
from pathlib import Path

import torch
from mobile_sam import sam_model_registry
from mobile_sam.utils.onnx import SamOnnxModel

source = Path(environ["MOBILE_SAM_SOURCE"])
output = Path(environ["MOBILE_SAM_OUTPUT"])
model = sam_model_registry["vit_t"](
    checkpoint=str(source / "weights/mobile_sam.pt")
).eval()


class Encoder(torch.nn.Module):
    def __init__(self, sam):
        super().__init__()
        self.encoder = sam.image_encoder

    def forward(self, images):
        return self.encoder(images)


with (output / "mobilesam-vit-t-encoder.onnx").open("wb") as handle:
    torch.onnx.export(
        Encoder(model).eval(),
        torch.randn(1, 3, 1024, 1024, dtype=torch.float32),
        handle,
        export_params=True,
        verbose=False,
        opset_version=17,
        do_constant_folding=True,
        input_names=["images"],
        output_names=["image_embeddings"],
        dynamo=False,
    )

decoder = SamOnnxModel(model=model, return_single_mask=False).eval()
embed_dim = model.prompt_encoder.embed_dim
embed_size = model.prompt_encoder.image_embedding_size
mask_input_size = [4 * value for value in embed_size]
dummy_inputs = {
    "image_embeddings": torch.randn(1, embed_dim, *embed_size, dtype=torch.float32),
    "point_coords": torch.randint(
        low=0, high=1024, size=(1, 5, 2), dtype=torch.float32
    ),
    "point_labels": torch.randint(low=0, high=4, size=(1, 5), dtype=torch.float32),
    "mask_input": torch.zeros(1, 1, *mask_input_size, dtype=torch.float32),
    "has_mask_input": torch.zeros(1, dtype=torch.float32),
    "orig_im_size": torch.tensor([1080, 810], dtype=torch.float32),
}
with (output / "mobilesam-vit-t-decoder.onnx").open("wb") as handle:
    torch.onnx.export(
        decoder,
        tuple(dummy_inputs.values()),
        handle,
        export_params=True,
        verbose=False,
        opset_version=17,
        do_constant_folding=True,
        input_names=list(dummy_inputs),
        output_names=["masks", "iou_predictions", "low_res_masks"],
        dynamic_axes={
            "point_coords": {1: "num_points"},
            "point_labels": {1: "num_points"},
        },
        dynamo=False,
    )
PY
  verify_file "$build_dir/mobilesam-vit-t-encoder.onnx" "$encoder_sha"
  verify_file "$build_dir/mobilesam-vit-t-decoder.onnx" "$decoder_sha"

  MOBILE_SAM_OUTPUT="$build_dir" "$environment_dir/bin/python" - <<'PY'
from os import environ
from pathlib import Path

import numpy as np
import onnxruntime as ort

output = Path(environ["MOBILE_SAM_OUTPUT"])
encoder = ort.InferenceSession(
    str(output / "mobilesam-vit-t-encoder.onnx"),
    providers=["CPUExecutionProvider"],
)
decoder = ort.InferenceSession(
    str(output / "mobilesam-vit-t-decoder.onnx"),
    providers=["CPUExecutionProvider"],
)
embedding = encoder.run(
    None, {"images": np.zeros((1, 3, 1024, 1024), dtype=np.float32)}
)[0]
outputs = decoder.run(
    None,
    {
        "image_embeddings": embedding,
        "point_coords": np.array([[[512, 512], [0, 0]]], dtype=np.float32),
        "point_labels": np.array([[1, -1]], dtype=np.float32),
        "mask_input": np.zeros((1, 1, 256, 256), dtype=np.float32),
        "has_mask_input": np.zeros((1,), dtype=np.float32),
        "orig_im_size": np.array([1080, 810], dtype=np.float32),
    },
)
if [value.shape for value in outputs] != [
    (1, 4, 1080, 810),
    (1, 4),
    (1, 4, 256, 256),
]:
    raise RuntimeError(
        f"unexpected MobileSAM CPU output shapes: {[value.shape for value in outputs]}"
    )
PY
  mv "$build_dir/mobilesam-vit-t-encoder.onnx" "$encoder_file"
  mv "$build_dir/mobilesam-vit-t-decoder.onnx" "$decoder_file"
  trap - EXIT HUP INT TERM
  rm -rf "$build_dir"
fi

(cd "$atlas_dir" && npm ci --ignore-scripts)

printf '%s\n' "MobileSAM browser assets are ready"
