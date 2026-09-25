# known_hosts 可能落在临时目录

- 状态：待办（M3 接入真实主机密钥确认之前必须定下）
- 记录日期：2026-09-25

## 现状

`native/hub/src/session.rs` 的 `known_hosts_path()` 先试 `$HOME/.guosh`，建目录失败就退到
系统临时目录。App 沙箱里容器根目录不一定可写；落在临时目录时文件可能被系统清理。
现在的 TOFU 自动接受会静默重新信任；M3 上真实确认后，就表现为「再次连接又弹确认」。

## 下一步

改用持久目录（`Library/Application Support`，可复用上游 `rshell_platform::PlatformPaths`）；
新位置从空文件开始，不迁移 TOFU 时期自动写入的条目，并清理旧位置。
