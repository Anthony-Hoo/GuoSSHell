#!/usr/bin/env python3
"""将 Actions Secrets 写为临时 Android 签名文件，失败或完成后清理。"""

import base64
import os
from pathlib import Path
import sys

KEYSTORE = Path("build/ci/android-release.p12").resolve()


def main():
    if sys.argv[1:] == ["clean"]:
        KEYSTORE.unlink(missing_ok=True)
        return
    value = os.environ.get("ANDROID_KEYSTORE_BASE64", "")
    if not value:
        if os.environ.get("GUOSH_CI_RELEASE") == "true":
            raise RuntimeError("版本发版必须配置 Android 发布签名 Secrets")
        print("未配置 Android 发布签名，本次仅生成开发签名包")
        return
    for key in ["ANDROID_KEY_ALIAS", "ANDROID_STORE_PASSWORD", "ANDROID_KEY_PASSWORD"]:
        if not os.environ.get(key):
            raise RuntimeError(f"缺少 {key}")
    data = base64.b64decode(value, validate=True)
    if not data:
        raise RuntimeError("签名文件为空")
    KEYSTORE.parent.mkdir(parents=True, exist_ok=True)
    with open(KEYSTORE, "wb") as output:
        os.chmod(KEYSTORE, 0o600)
        output.write(data)
    with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as output:
        output.write(f"ANDROID_KEYSTORE_PATH={KEYSTORE}\n")
    print("Android 签名文件已就绪")


if __name__ == "__main__":
    main()
