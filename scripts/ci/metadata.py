#!/usr/bin/env python3
"""校验版本标签，统一所有平台的版本、提交和构建矩阵。"""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
CONFIG = json.loads((ROOT / "scripts/ci/config.json").read_text())
VERSION = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
TAG = re.compile(r"v" + VERSION + r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z")


def metadata(ref, sha, pubspec, now=None):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("提交标识必须是完整 SHA")
    release = ref.startswith("refs/tags/")
    prerelease = False
    if release:
        tag = ref.removeprefix("refs/tags/")
        matched = TAG.fullmatch(tag)
        if not matched:
            raise ValueError("版本标签必须为 vX.Y.Z 或 vX.Y.Z-预发布标识")
        suffix = matched.group(4)
        if suffix and any(part.isdigit() and len(part) > 1 and part[0] == "0" for part in suffix.split(".")):
            raise ValueError("预发布数字标识不能包含前导零")
        build_name = ".".join(matched.groups()[:3])
        version = tag[1:]
        prerelease = bool(suffix)
    else:
        matched = re.search(r"^version:\s*([0-9]+\.[0-9]+\.[0-9]+)", pubspec, re.M)
        if not matched:
            raise ValueError("pubspec.yaml 缺少版本号")
        build_name = matched.group(1)
        version = f"{build_name}-dev.{sha[:12]}"
    # 两个仓库使用同一时间基准，避免各自的 run_number 使 Android 更新版本倒退。
    number = int(time.time() if now is None else now) - 1577836800
    if not 1 <= number < 2_000_000_000:
        raise ValueError("构建号超出移动平台允许范围")
    return {
        "version": version,
        "build_name": build_name,
        "build_number": str(number),
        "release": str(release).lower(),
        "prerelease": str(prerelease).lower(),
        "commit": sha,
        "matrix": json.dumps({"include": CONFIG["targets"]}, separators=(",", ":")),
    }


def emit(values):
    text = "".join(f"{key}={value}\n" for key, value in values.items())
    if "GITHUB_OUTPUT" in os.environ:
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    if sys.argv[1:] == ["tools"]:
        emit({key: value for key, value in CONFIG.items() if key != "targets"})
    else:
        commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        emit(metadata(os.getenv("GITHUB_REF", "refs/heads/local"), commit, (ROOT / "pubspec.yaml").read_text()))
