# UPSTREAM.md —— 上游来源、依赖方式与许可

本项目是**基于 rsHell 改出来的独立仓库**。上游内核不在本仓库里，而是作为
**git 依赖**被 pin 到一个 commit——这个 commit 在我们的 fork 上：上游基线之上
叠了几个小补丁（见文末「fork 补丁」），每一个都写明改了哪几行、为什么。

| 项 | 值 |
|---|---|
| 上游仓库 | https://github.com/hugefiver/rsHell |
| 上游基线 commit | `b2ab8656079225dc2c920c24f5d9e0124f4f83e1` |
| fork 仓库 | https://github.com/Anthony-Hoo/rsHell（`guosh` 分支） |
| pin 住的 commit | `d15ca5d898d3ad2c008a1bacdc1ed0045c46ffb9`（基线 + 下文补丁） |
| 许可证 | MIT，Copyright (c) 2026 hugefiver（副本见 `LICENSES/rsHell-MIT.txt`） |

依赖声明在 `Cargo.toml`：

```toml
rshell-core = { git = "https://github.com/Anthony-Hoo/rsHell", rev = "d15ca5d898d3ad2c008a1bacdc1ed0045c46ffb9" }
rshell-session = { git = "https://github.com/Anthony-Hoo/rsHell", rev = "d15ca5d898d3ad2c008a1bacdc1ed0045c46ffb9" }
rshell-storage = { git = "https://github.com/Anthony-Hoo/rsHell", rev = "d15ca5d898d3ad2c008a1bacdc1ed0045c46ffb9" }
```

升级上游 = 在 fork 里把 `guosh` 分支 rebase 到新基线（补丁都很小，冲突面可控），
跑上游 `rshell-core` / `rshell-session` 的测试与我们的回归，再换这里的 rev。

## 为什么是 git 依赖，而不是 vendor 进仓库

一开始是把上游四个 crate 的源码 vendor 进 `rust/upstream/`（211 个文件 / 1.8 MB）。
改用 git 依赖的原因：

- 仓库干净：本仓库只装**我们自己的**代码。
- provenance 就写在一行 `rev = "..."` 里，比一个副本目录更难说谎。
- 对上游的改动收在 fork 上、逐条记录（见文末「fork 补丁」），不和我们自己的代码混在一起。

代价（必须知道）：

1. **全新环境首次构建需要网络。** cargo 要把仓库 clone 进 `~/.cargo/git/`。
   离线机器要先 `cargo fetch`。
2. **改上游要走 fork。** 需要改上游时在 fork 的 `guosh` 分支上加提交、换 `rev=`，
   并在文末「fork 补丁」记一笔。

## 不需要改上游就能解决的三件事

### 1. iOS 的 keyring `protected` feature：不需要改上游

上游 `rshell-storage` 用 `keyring 4.1.5`，其 `v1` feature 在 iOS 上只启用
`apple-native-keyring-store/keychain`，而 iOS 没有 legacy keychain，只有
protected data store，于是直接编译失败：

```
error: The `protected` feature is required on iOS
```

之前为了过这一关去改了上游的 `Cargo.toml`。**现在不用了**：Cargo 的 feature 是
按**包**统一（unification）的，只要依赖图里**任何一个人**打开了它，
`rshell-storage` 那条路径也会看到。所以在我们自己的 `Cargo.toml` 里加：

```toml
[target.'cfg(target_os = "ios")'.dependencies]
apple-native-keyring-store = { version = "1.0.1", features = ["protected"] }
```

实测有效：`cargo check --target aarch64-apple-ios` 通过，不用为此改上游。

### 2. `[patch.crates-io] portable-pty-psmux`：我们不需要

上游根 crate 有一个指向 `third_party/portable-pty-psmux` 的 patch。
读了那份目录里的 `README.rshell-patch.md` 才知道：rsHell 对它的改动**全部在
`src/win/*`**（Windows ConPTY 的 Job handle 处理）加一个只给 dev-dependencies 用的
`containment-test-support` feature。**我们的目标是 iOS / macOS，这些文件根本不参与编译。**

而且这里还藏着一个必然性：`rshell-session` 的 `[dev-dependencies]` 里引用了
`containment-test-support` 这个 feature，而它在 crates.io 上的
`portable-pty-psmux 0.9.6` 里不存在。**cargo 会解析 path 依赖的 dev-dependencies**，
所以只要 `rshell-session` 是 path 依赖，去掉 patch 就会直接报：

```
package `rshell-session` depends on `portable-pty-psmux` with feature
`containment-test-support` but `portable-pty-psmux` does not have that feature.
```

换成 **git 依赖后这个问题自己消失了**（cargo 不解析 git 依赖的 dev-dependencies）。
实测：去掉 patch、纯 git 依赖，`cargo check --target aarch64-apple-ios` 通过。
补丁说明留档在 `LICENSES/portable-pty-psmux-PATCH-NOTES.md`。

### 3. iOS 不可用的传输：靠链接器裁掉

链接出来的 iOS 可执行文件**会导入** `_openpty` / `_login_tty` / `_fork` /
`_posix_spawnp` 等符号——它们来自 `rshell-session` 里那三个 iOS 不可用的传输
（`local` / `pty` / `system_ssh`），我们从不调用，但**符号被保留下来了**。

**这不影响交付**：加 `-Wl,-dead_strip`（Xcode 的
`DEAD_CODE_STRIPPING = YES`，Release 默认开）后这些导入**全部消失**，
二进制从 13 MB 降到 3.7 MB。实测见 `PLAN.md` §3.2。

如果将来需要把这些传输从编译图里彻底摘掉（而不是靠链接器裁），就在 fork 上用
feature gate 关掉 `transport/local.rs` + `pty.rs`，并在文末「fork 补丁」记一笔。

## 顺带：`LICENSES/`

| 文件 | 内容 |
|---|---|
| `rsHell-MIT.txt` | 上游 rsHell 的 MIT 许可（Copyright (c) 2026 hugefiver） |
| `portable-pty-psmux-MIT.md` | `portable-pty-psmux` 的 MIT 许可（Wez Furlong） |
| `portable-pty-psmux-PATCH-NOTES.md` | 上游那份 patch 的说明，留档——我们现在不应用它了 |

MIT 要求「许可声明随软件或其重要部分一起分发」。App Store 提交时需要一份
第三方许可清单，这三个文件就是它的起点。

## fork 补丁（`guosh` 分支，按提交顺序）

| # | 提交 | 改了什么 | 为什么 |
|---|---|---|---|
| P1 | `103a00b` feat(core): expose bracketed paste in terminal display modes | `rshell-core/src/render.rs`：`TerminalDisplayModes` 加 `bracketed_paste`（`#[serde(default)]`）；`rshell-session/src/alacritty_display.rs`：由 `TermMode::BRACKETED_PASTE` 填充；两处测试字面量补字段、`engine_contract.rs` 加用例 | 粘贴要按远端是否开启 DECSET 2004 包 `ESC[200~ … ESC[201~`，这个模式原本只在 alacritty 适配层内部可见。不计入 `has_residue()`、恢复序列也不重置它——shell 在每个提示符都会打开它 |
| P2 | `a2730aa` feat(core): allow password profiles without a saved credential | `rshell-core/src/connection/validation.rs`：Password 认证不再强制 `credential_ref`；`connection_catalog.rs` 与存储层 `credentials.rs` 的对应用例改为新语义 | 「不保存密码、连接时再问」是必要能力；没存密码时 App 弹框问、用 `AuthPlan::from_secret` 连接。清除已存密码因此合法，并删除钥匙串里的条目 |
| P3 | `50c0f9b` feat(session): optional prompt to replace a changed host key | `rshell-session/src/host_keys.rs`：`KnownHostsVerifier::with_changed_key_prompt()`；`host_keys/storage.rs`：`replace` 按 russh 的条目编号（不计注释行）去掉该 host:port 的旧条目再写新键；`tests/host_keys.rs` 加两个用例 | 密钥变更时要给出显式警告并允许用户确认后替换（PLAN §5 M3）。默认关闭：上游的「变更即失败、不提示」语义与测试不变；拒绝仍是 `HostKeyChanged` |
| P4 | `c794280` feat(session): authenticate with a decoded private key held in memory | `rshell-session/src/auth.rs`：`AuthPlan::PrivateKey{host, key}` 与 `AuthPlan::from_private_key`（只接受 PublicKey 认证的配置，不读 `identity_file`）；`transport/native_ssh/auth.rs`：该变体直接做公钥认证，与读文件的路径共用 `authenticate_with_key`（RSA 按服务器支持选 SHA-2 签名）；`tests/auth.rs`、`tests/ssh_smoke.rs` 各加一个用例 | 私钥存在钥匙串、口令由 App 询问并解密，私钥只在内存里；上游只能从 `identity_file` 读磁盘文件 |
| P5 | `64233db` feat(session): authenticate through an external signer | `rshell-session/src/auth.rs`：`ExternalSigner` trait（签名、返回 SSH 签名 blob）、`ExternalSignerError`、`AuthPlan::Signer{host, public_key, signer}` 与 `AuthPlan::from_signer`（只接受 PublicKey 认证的配置）；`transport/native_ssh/auth.rs`：该变体经 russh 的 `authenticate_publickey_with` 认证，适配器把签名 blob 作为 SSH string 接在待签数据之后，签名器失败按认证失败报；`tests/auth.rs` 加一个用例，`tests/ssh_smoke.rs` 加两个（外部签名器认证通过、签名器失败即认证失败） | 私钥在硬件卡（OpenPGP 卡）或平台认证器里，进程拿不到私钥，只能请它签名 |
| P6 | `bb38b1f` feat(session): bound connecting separately from other operations | `rshell-session/src/transport/native_ssh.rs`：`NativeSshTransport::with_connect_timeout`，只作用于 `connect`（TCP、握手、主机密钥确认与认证，含等交互的时间），不设时仍用操作超时；`tests/native_ssh.rs` 加一个用例（问答等待超过操作超时、未超连接上限时照常连上） | 上游的操作超时把用户看指纹、输 PIN、按卡的时间也算进连接里，慢一点就「连接超时」。App 自己只给等网络的时间计时（等用户时暂停），交给传输层的连接上限放宽 |
| P7 | `4ff71e5` feat(session): give up connections whose keepalives go unanswered | `rshell-session/src/transport/native_ssh.rs`：`NativeSshTransport::with_keepalive(interval, max)`，把 russh 的 `keepalive_interval` / `keepalive_max` 交给调用方，不设时不发 keepalive（上游行为不变）；`tests/native_ssh.rs` 加一个用例（经可冻结的 TCP 代理，对端静默后以网络失败结束，而不是一直挂着） | 上游不发 keepalive：设备休眠、换网络后对端悄悄没了，会话会一直挂在「已连接」。App 设置间隔与次数，死连接在一分钟内以「连接已断开」结束，可以重连 |
| P8 | `d15ca5d` feat(session): end synchronized updates at their deadline | `rshell-session/src/engine.rs`：`TerminalEngine::sync_deadline()` / `end_sync()`（默认实现为空）；`alacritty_adapter.rs` / `alacritty_feed.rs`：给出 vte 解析器缓冲同步输出的截止时刻，`end_sync` 把缓冲的输出按常规的滚回记账画上去，截止之后到的输出先结束同步再解析（喂字节的记账抽成 `apply_window` 共用）；`tests/engine_contract.rs` 加三个用例 | 同步输出（DEC 2026）期间 vte 缓冲全部输出直到结束序列，终端在短超时后放弃等待（alacritty 150 ms），上游没有这一步：程序在一帧中途被杀，之后的输出连同 shell 提示符都被扣住，画面卡死。会话循环按截止时刻结束同步 |
