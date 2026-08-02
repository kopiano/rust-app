#!/usr/bin/env bash
set -euo pipefail

cd /workspace/GPT-SoVITS

PRETRAINED_DIR="${GPT_SOVITS_PRETRAINED_DIR:-/workspace/GPT-SoVITS/GPT_SoVITS/pretrained_models}"
REPO_ID="${GPT_SOVITS_MODEL_REPO:-lj1995/GPT-SoVITS}"
REVISION="${GPT_SOVITS_MODEL_REVISION:-main}"

required_paths=(
  "chinese-roberta-wwm-ext-large/config.json"
  "chinese-hubert-base/config.json"
  "gsv-v2final-pretrained/s1bert25hz-5kh-longer-epoch=12-step=369668.ckpt"
  "gsv-v2final-pretrained/s2G2333k.pth"
)

missing=0
for relative_path in "${required_paths[@]}"; do
  if [[ ! -f "${PRETRAINED_DIR}/${relative_path}" ]]; then
    missing=1
    break
  fi
done

if [[ "${missing}" == "1" ]]; then
  echo "GPT-SoVITS V2 pretrained assets are missing; downloading ${REPO_ID}@${REVISION}."
  python /bridge/download_pretrained.py \
    --repo-id "${REPO_ID}" \
    --revision "${REVISION}" \
    --destination "${PRETRAINED_DIR}"
fi

exec python api_v2.py \
  -a "${GPT_SOVITS_BIND_ADDR:-0.0.0.0}" \
  -p "${GPT_SOVITS_PORT:-9880}" \
  -c "${GPT_SOVITS_TTS_CONFIG:-GPT_SoVITS/configs/tts_infer.yaml}"
