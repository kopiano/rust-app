import argparse
from pathlib import Path

from huggingface_hub import snapshot_download


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo-id", required=True)
    parser.add_argument("--revision", default="main")
    parser.add_argument("--destination", required=True)
    args = parser.parse_args()

    destination = Path(args.destination)
    destination.mkdir(parents=True, exist_ok=True)
    snapshot_download(
        repo_id=args.repo_id,
        revision=args.revision,
        local_dir=destination,
        allow_patterns=[
            "chinese-roberta-wwm-ext-large/**",
            "chinese-hubert-base/**",
            "gsv-v2final-pretrained/s1bert25hz-5kh-longer-epoch=12-step=369668.ckpt",
            "gsv-v2final-pretrained/s2G2333k.pth",
        ],
    )


if __name__ == "__main__":
    main()
