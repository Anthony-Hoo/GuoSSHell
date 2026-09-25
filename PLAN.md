# PLAN.md — GuoSSHell

> 把 rsHell 的 **Rust 业务内核**原样搬到一个 Flutter 前端的 iOS / iPadOS 应用上。
> 这份文件是实现期的唯一参考。所有结论都标注了证据来源；标「实测」的都是本机跑出来的，可复现。

- 上游基线：`hugefiver/rsHell` @ `b2ab8656079225dc2c920c24f5d9e0124f4f83e1`（2026-09-14，MIT），
  经 fork（`Anthony-Hoo/rsHell` 的 `guosh` 分支：基线 + 小补丁）**作为 pin 住 rev 的 git 依赖**
  （上游源码不进本仓库；补丁逐条记在 `rust/UPSTREAM.md`）。
  **改上游的原则**：能不改就不改；设计上明显不合理、或挡住必要能力时就改，按 §9.2 第 3 条
  的规则走（在 fork 的 `guosh` 分支加提交、换 `rev`、在 UPSTREAM.md 记录改了哪几行、为什么）。
- 应用名：**GuoSSHell**
- 开发环境：macOS（Apple Silicon）· Xcode 16+ · Flutter 3.x
  （**本机的具体版本号、工具绝对路径、真机清单、签名配置等一律不入库**，见 §7）
- 相关文档：
  - `docs/feasibility-2026-09-14.html`（可行性：alacritty 四层判定、rinf 事实纠正）
  - `docs/mvp-plan-2026-09-14.html`（里程碑与 M0 交接单）
  - `rust/`（可运行的 M0 代码 + `bench_frame` 性能基准 + `UPSTREAM.md`）

---

## 0. 铁律

1. **业务与终端语义全部跑在 Rust**，能不改就不改。Dart **不写业务逻辑**。
2. **只有渲染、交互这些必不可少的部分用 Flutter 重写。**
3. **优先复用，绝不造轮子。** 具体顺序是：
   1. pub.dev / crates.io 有能用的 → 直接用；
   2. 有接近的但不够好 → **在已有开源项目上 fork**，不要从零写；
   3. 只有确认前两条都不成立才自己写，并且要在本文件里写下「为什么没有可复用的」。
   这条对每一层都适用：渲染、输入、网络发现、连接存储，全都先找现成的。
4. **Rust 是终端状态的唯一权威。** Dart 侧不得持有第二份终端状态机——这条是选技术方案的硬约束，
   任何让 Dart 也解析一遍 ANSI 的方案（例如直接用 `xterm.dart` 的 `Terminal`）都直接排除。
5. **所有产物都放在 `GuoSSHell/` 里。** 不要在存放多个项目的**容器目录**里留散落文件。
   仓库结构：Flutter app 在根，Rust 一律在 `rust/`（**不要占用 `ios/`**，那是 Flutter 自己的目录）。
   临时文件（探针、半成品克隆）用 `/tmp`，用完即走。

---

## 1. 项目定义

**GuoSSHell = 一个纯 SSH 客户端的 iOS / iPadOS 版本。**

明确不做的事：**不做本地 shell 面板**。上游 rsHell 的「本地终端」是一等公民，
但 iOS 上不可能（没有 `fork`、没有 `openpty`、沙箱禁止 spawn 进程），所以 iOS 版
把产品模型收窄为「远端 SSH 终端」。Rust 侧零改动——只要 Flutter 侧不暴露
`UiCommand::NewLocalTab` / `UiCommand::StartLocal` 就行。

上游是 MIT，fork 与改名合规，但**必须保留原始版权与许可声明**。

---

## 2. 架构

三层，边界已经存在于上游代码里，**不需要重新设计**。

```
┌──────────────────────────────────────────────────────────────┐
│ Flutter（全量重写，约 2.7 万行规模）                            │
│  终端画布 / 选择 / IME / 软键盘 / 连接表单 / 侧栏 / 标签 / 分屏  │
└──────────────┬───────────────────────────────────────────────┘
               │  rinf：单向信号流（Dart → Rust 命令，Rust → Dart 事件）
               │  ⚠ 边界上的类型全部来自 rshell-core::protocol，不自己定义
┌──────────────┴───────────────────────────────────────────────┐
│ Rust（保留，约 1.1 万行）                                       │
│  rshell-core     4,240 行  领域模型 / 连接目录 / 设置 / 渲染帧类型 │
│  rshell-session  7,036 行  SSH 传输 / alacritty 适配 / 引擎 / actor│
│  ─────────────────────────────────────────────────────────────│
│  rshell-platform 1,306 行  → 需要 iOS 分支                      │
│  rshell-storage  4,785 行  → SQLite 可用；keyring 走 protected  │
└──────────────┬───────────────────────────────────────────────┘
               │  russh + tokio TCP
┌──────────────┴───────────────────────────────────────────────┐
│ 远端主机（PTY 与 shell 都在这一侧）                              │
└──────────────────────────────────────────────────────────────┘
```

### 2.1 边界已经存在（这是整件事能成立的原因）

`rshell-core::protocol` 里已经有一整套可序列化的命令/事件：

| 层级 | 类型 | 用途 | serde |
|---|---|---|---|
| 应用级 | `UiCommand` / `AppEvent` | 连接目录、搜索、设置、导入、分屏、标签、PaneId | 已有 |
| 会话级 | `SessionUiCommand` / `SessionUiEvent` | 输入、鼠标、粘贴、缩放、滚动、搜索、选择、交互应答 | 已有 |
| 交互 | `InteractionRequest` / `InteractionResponse` / `HostKeyPrompt` | 主机密钥确认、密码、私钥口令、keyboard-interactive | 请求已有；**响应刻意没有**（见下） |
| 渲染 | `RenderFrame` / `RenderRow` / `RenderCell` / `TerminalSize` / `Viewport` | 帧数据 | 已有 |

上游的 GTK 层（`rshell-ui`，22,812 行）就是靠这套类型跟内核说话的。**换掉 GTK，边界不用动。**

`SessionClient` 的形状也天然对齐 rinf：

```rust
pub struct SessionClient {
    pub commands: mpsc::Sender<SessionCommand>,          // → DartSignal
    pub events: broadcast::Receiver<SessionEvent>,        // → RustSignal
    pub frames: watch::Receiver<Option<Arc<RenderFrame>>>,// → 只留最新，正好是丢帧背压
}
```

### 2.2 两个边界上的硬约束

1. **秘密不能序列化。** `SessionUiCommand::Paste(SecretString)` 与
   `InteractionResponse::Secret(SecretString)` 刻意**没有** serde 派生。
   Dart 侧只能传明文，Rust 侧在接收处自己包成 `SecretString`。
   这意味着这一层协议要手写，不能全 `derive`。
2. **`serde` 必须开 `rc` feature。** `RenderFrame.rows` 与 `RenderRow.cells` 的类型是
   `Arc<[..]>`，serde 默认不给 `Arc` 实现序列化。（实测：不开就在 `encode` 处报 trait 不满足。）

---

## 3. 关键事实清单（实测，不是推测）

### 3.1 传输层在 iOS 上的可达性

| 传输 | 位置 | iOS | 原因 |
|---|---|---|---|
| `native_ssh`（russh） | `transport/native_ssh.rs` | <span>✅ 可用</span> | 纯 `russh` + `tokio::net::TcpStream`。PTY 开在**远端**，本地零 `fork` |
| `system_ssh` | `transport/system_ssh.rs` | ❌ 不可用 | 靠 `build_system_ssh_argv` 启动系统 `ssh`。沙箱禁止 spawn |
| `local` | `transport/local.rs` | ❌ 不可用 | portable-pty，需要 `fork` + `openpty` |
| `pty` | `transport/pty.rs` | ❌ 不可用 | 同上，是 `local` 的底层 |

**关键点**：这三个不可用的传输都在 `rshell-session` 内部，**不需要改动或删除**。
Flutter 侧不暴露入口即可。

### 3.2 编译与链接

- `alacritty_terminal 0.26.0` 全链为 `aarch64-apple-ios` **零改动编译通过**。
- `rshell-core` / `rshell-platform` / `rshell-storage` / `rshell-session` 为 iOS 编译通过，
  **不需要为编译改上游**。两处曾经以为必须改上游的地方，都在我们这边解决：
  1. keyring 的 `protected` feature —— 在我们自己的 `Cargo.toml` 里加一条
     `[target.'cfg(target_os = "ios")'.dependencies] apple-native-keyring-store = { features = ["protected"] }`，
     靠 Cargo 的 **feature unification** 生效。不改上游的 `Cargo.toml`。
     不这么做会直接 `error: The 'protected' feature is required on iOS`。
  2. 上游的 `[patch.crates-io] portable-pty-psmux` —— 它的改动**全在 `src/win/*`**
     与一个只给 dev-dependencies 用的 feature，我们的目标根本不编译这些文件。
     而且换成 git 依赖后，cargo 不再解析依赖的 dev-dependencies，那个 feature 冲突自己消失。
  详见 `rust/UPSTREAM.md`（含证据与代价）。

- **链接可行性已经用命令行验过，不需要 Xcode**（`./scripts/link-check.sh`）：
  两个切片各自链进一个 iOS 可执行文件，**唯一的额外链接标志是 `-liconv`**。

- **PTY 家族的符号需要 `-Wl,-dead_strip` 才会消失 —— 这一条修正了早先的结论。**
  早先的审计方法是错的：对 `.a` 跑 `nm -u`，而 Apple 的 `nm` 读不了 Rust 1.89/LLVM20
  产出的部分目标文件（`Invalid attribute group entry`），于是**静默返回空结果**，
  被误读成「命中数 0」。改用「链接成可执行文件后再审计」这个可信方法，实测：

  | | 无 dead strip | 有 `-Wl,-dead_strip` |
  |---|---|---|
  | `_openpty` / `_login_tty` / `_fork` / `_posix_spawnp` | 各 1 | **0** |
  | `posix_spawn*` 家族 | 9 | **0** |
  | 未解析符号总数 | 280 | 103 |
  | 可执行文件体积 | 13 MB | **3.7 MB** |

  这些符号来自 `transport/local.rs` / `pty.rs` / `system_ssh.rs` 三个 iOS 上从不调用的传输，
  代码是死的，但符号被保留。`-Wl,-dead_strip` 就是 Xcode 的
  `DEAD_CODE_STRIPPING = YES`（Release 默认开）。
  **如果以后要把它们从编译图里彻底摘掉**，就得 fork 上游加 feature gate —— 那是 git 依赖的
  第一个真实代价，记在 §9.2。

- 体积（iOS device，git 依赖 + release）：

  | 产物 | 大小 |
  |---|---|
  | `librshell_m0.a`（release，未 strip） | 65 MB |
  | 链进 App 后（`-dead_strip`）的可执行文件 | **3.7 MB** ← Rust 内核真实增量 |
  | 同上，但不加 dead strip | 13 MB |

### 3.3 M0 端到端（不需要任何外部服务器）

`rshell-ios-m0/examples/m0_loopback.rs` 在同进程里起一个最小 russh 服务端，
用 rsHell 自己的 `NativeSshTransport` 走完整路径。实测 transcript：

```
PTY-ACK term=xterm-256color cols=80 rows=24 px=0x0
M0-LOOPBACK-SHELL-OK
$ echo M0-ECHO
```

证明：TCP + 握手 + 主机密钥 TOFU 落盘 + 密码认证 + `request_pty(真实参数)` +
`request_shell` + **写路径** + 读路径，全部通。

### 3.4 代码规模

| 位置 | 行数 | iOS 处置 |
|---|---|---|
| `rshell-core` | 4,240 | 全量保留 |
| `rshell-session` | 7,036 | 保留（去掉三个不可用传输） |
| `rshell-platform` | 1,306 | 需要 iOS 分支 |
| `rshell-storage` | 4,785 | 基本保留 |
| **`rshell-ui`** | **22,812** | **全部重写** |
| 根 `src/` | 4,215 | **全部重写**（业务逻辑在 `rshell-core::application`，1,853 行） |
| 合计 | 44,394 | 保留 11,276（25%），重写 27,027（61%） |

> **要重写的 Flutter 层是它对话的 Rust 内核的 2.4 倍。** 这是这个项目的真实形状。

---

## 4. 性能预算（回答「整帧传 RenderFrame 会不会有瓶颈」）

实测命令：`cd rust && cargo run --release --example bench_frame`

### 4.1 iPad 横屏 120×40（4,800 cells/帧，主循环预算 16.67 ms）

| 负载 | `render()` | run/行 | run 总数 | serde_json | bincode | run 打包 | 脏 3 行 |
|---|---|---|---|---|---|---|---|
| `ls --color` | 0.14 ms | 5.0 | **200** | 836.5 KB | 52.9 KB | **7.2 KB** | **0.5 KB** |
| `git diff` | 0.11 ms | 1.8 | **73** | 837.1 KB | 52.6 KB | **5.8 KB** | **0.4 KB** |
| TUI 满屏（最坏） | 0.11 ms | 120.0 | **4,800** | 848.5 KB | 56.4 KB | **61.2 KB** | 4.6 KB |
| CJK 日志 | 0.10 ms | 2.0 | **80** | 699.1 KB | 44.8 KB | **6.6 KB** | 0.5 KB |

编码耗时：bincode 0.06–0.09 ms · run 打包 0.03–0.13 ms。

### 4.2 iPhone 竖屏 40×22（880 cells/帧）

| 负载 | `render()` | run 总数 | serde_json | bincode | run 打包 | 脏 3 行 |
|---|---|---|---|---|---|---|
| `ls --color` | 0.02 ms | 26 | 154.3 KB | 9.6 KB | 1.3 KB | 0.2 KB |
| TUI 满屏 | 0.02 ms | 880 | 156.6 KB | 10.4 KB | 11.3 KB | 1.5 KB |
| CJK 日志 | 0.02 ms | 43 | 109.7 KB | 7.3 KB | 1.7 KB | 0.2 KB |

### 4.3 结论

**瓶颈存在，但不在你担心的位置。**

1. **Rust 侧不是瓶颈。** `render()` + 编码总计 **< 0.3 ms**，预算 16.67 ms，余量 50× 以上。
   即使 `RenderCell.text: String` 会给每个 cell 分配一次堆内存（4,800 次/帧），也不够看。
2. **`serde_json` 是禁区。** 700–850 KB/帧，因为每个空格都要写成 `" "` 加字段名。
   60 fps 就是 50 MB/s。**绝对不要用它做帧传输。**
3. **bincode 能用但浪费。** 固定 ~50 KB/帧，因为它是无压缩的逐 cell 编码，
   `RenderCell` 的每个字段（含 `String` 长度前缀）都要过一遍线。60 fps ≈ 3.2 MB/s。
4. **真正的成本在 Dart 侧的对象图。** 逐 cell 传 4,800 个对象，Dart 每帧要建
   ~4,800 个小对象 + 做 ~4,800 次文本布局。60 fps 下是 **28.8 万对象/秒**，
   GC 和文本布局会被打爆。**这才是「整帧传会不会有瓶颈」的正确答案。**
5. **解法：run 压缩 + 脏行增量。** 把相邻同属性单元格合并成一个 run 再打包：

   | | 典型 shell 输出 | TUI 满屏（最坏） |
   |---|---|---|
   | Dart 侧要建的对象数 | 4,800 → **73–200**（**24×–65×**） | 4,800（无法压缩） |
   | 每帧字节数 | 52.9 KB → **5.8–7.2 KB**（**7×–9×**） | 61.2 KB |
   | 再加脏行增量 | **0.4–0.5 KB** | 4.6 KB |

   TUI 满屏无法压缩是物理事实（每个 cell 独立底色），但它也几乎不会以 60 fps 刷新
   ——`htop` 默认 1.5 s 一次。真正的 60 fps 场景是 `tail -f`、编译日志、
   `ls` 之类，恰好都是 run 压缩效果最好的形状。

6. **落地方案：结构化帧 + run 压缩 + 脏行增量，走 rinf 的 `RustSignalBinary` 原始字节通道。**

   - 不改 `rshell-core` 的 `RenderFrame` 契约（它是上游的核心资产）。
   - 在 iOS 侧的适配层加一个「`RenderFrame` → 紧凑 run 字节流」编码器。
   - **不需要自己设计序列化框架**：`RustSignalBinary` 就是 rinf 为这种情况准备的逃生舱。
   - 位图方案（Rust 侧渲染成像素）**不采用**：一旦 rinf 丢帧就是花屏（必须带
     generation 号重传），文本不可选，字体要内嵌进 Rust，DPI 变化要重渲。
     run 压缩后已经落在 7 KB/帧量级，没有理由付这些代价。

---

## 5. 里程碑

每个里程碑只增加**一类**未知量。前一个是绿的前提下才做下一个。**验收顺序：iPad 先通，再谈 iPhone / iOS。**

### M0 — SSH 直连 —— ✅ 已完成

写死地址和密码，先证明 SSH 本身在 iOS 上可行。无渲染、无输入、无 UI。

- **M0a（macOS 命令行，不需要 Xcode）** —— <span>已完成</span>
  `cargo run --example m0_loopback`（不需要外部服务器）
  `cargo run --example m0 -- <host> <port> <user> <password>`（打真实服务器）
- **M0b（在 iOS App 里跑起来）** —— <span>已完成</span>
  宿主用 `ios-host/` 里的 Swift 壳。产物 `librshell_m0.a` **不是 App，必须链进一个
  iOS App 目标才能跑**。

  实际跑通的方式：运行目标选 **"My Mac (Designed for iPad)"**（iPad 版二进制跑在 macOS 上、
  走 iOS 沙箱），手填地址 + 用户名密码，**在内网 SSH 服务器上验证通过**。
  建工程的完整步骤与四个坑见 `ios-host/README.md`。

  ⚠ 有三个**不满足就根本跑不起来**的前提，全在 `ios-host/README.md` 里展开：
  1. **iOS platform 必须单独下载。** Xcode 16 把平台支持拆出了 Xcode.app：SDK 随 Xcode 装
     （`-showsdks` 能看到），但**能被选中的 destination 要另外下**。没下之前
     `-showdestinations` 报 `iOS x.y is not installed`，运行目标里**一个 iOS 选项都没有**
     —— 包括下面第 2 条。`xcodebuild -downloadPlatform iOS`（约 7–8 GB，含模拟器 runtime）。见 §10.12。
  2. **"My Mac (Designed for iPad)" 只对纯 iOS target 出现。** 工程若用了 Xcode 的
     "Multiplatform App" 模板（`SDKROOT = auto` + `SUPPORTED_PLATFORMS` 含 `macosx`），
     Mac 上会直接编成**原生 macOS App**，这个选项**永远不会出现**。见 §10.13。
  3. **Run Script 必须声明 Output Files**，否则 User Script Sandboxing 会拦住脚本
     `cp` 到 `$(BUILT_PRODUCTS_DIR)`。见 §10.16。

  > **M0b 是必经之路吗？不是「必经」，但它是这条路线里最便宜的止损点。**
  > 它验证的是「**Rust 内核在一个真实 iOS App 里能不能跑**」，与 Flutter/rinf 无关。
  > 跳过它直接做 M1，一旦失败就同时有两个嫌疑人（Rust 侧 / rinf 集成），
  > 而 M0b 花的是十几分钟 Xcode 点击。
  >
  > **验证完能删吗？可以，且已经删了（2026-09-16，M1 全绿后退役）**——
  > 仓库内的 `ios-host/` 与 `scripts/link-check.sh` 已移除；仓库外的 Xcode 工程
  > （`~/Projects/Darwin/GuoSSHell`）由用户自行删除。踩坑知识保留在 §10 与 git 历史。

  > 诚实标注：模拟器与 "Designed for iPad" 跑在 macOS 用户态，**不能**用来证明
  > 「本地 PTY 在 iOS 上不工作」这类结论（会给出错误答案，见 §10.1）。
  > 但它们**可以**证明「iOS App 沙箱内、russh 出站 TCP 全链路能通」——
  > 因为这里唯一涉及平台差异的就是 socket 与 App 沙箱。
  > 真机与模拟器的差异（蜂窝、后台挂起、内存上限）留到 M1 与 M4 处理。

**验收**：App 内拿到远端提示符；失败时返回明确的 `SessionFailure` 分类
（`Network` / `Authentication` / `HostKey` / `SshChannel`）；写路径回显成功；
`known_hosts` 在 App 沙箱内落盘，第二次连接不再触发主机密钥交互。

**子项 M0-c：本地网络连接（内网）** —— <span>已完成</span>

- 公网机优先做，但内网连接是迟早要有的。
- **本期范围：手工填内网地址 + 权限处理 + 失败提示，不做自动发现**（见 §9）。
- 需要 `NSLocalNetworkUsageDescription`。**Xcode 16 的正规做法不是在 `Info.plist` 里写 key**
  —— 工程开了 `GENERATE_INFOPLIST_FILE` 时那个 plist 文件常常是空的 —— 而是在 target 的
  Build Settings 里设 `INFOPLIST_KEY_NSLocalNetworkUsageDescription`，Xcode 会把它合进
  最终 Info.plist。
  `NSBonjourServices` 只在用 Bonjour **发现**时才必需；本期不做自动发现，所以**不要加**。
- **iOS 没有「主动申请本地网络权限」的 API**，权限只能被第一次实际访问触发；
  未授权时 `connect()` **静默超时而不是报错**，所以代码必须把超时当作
  「可能没授权」处理，并提供跳转设置页的引导。这个引导要找现成实现复用，
  **不要自己写**（见 §6.3）。

### M1 — 一帧终端画面 —— ✅ 已完成（2026-09-16；子项 M1-b 除外）

字节 → `DefaultTerminalEngine::advance()` → `render(viewport, selection)` →
`RenderFrame` → run 压缩 → rinf 信号 → Flutter 画出来。**只读**，不处理输入。

> **起手式**：用 `rinf template` 在仓库根铺 Flutter 骨架（本机 `rinf` CLI 已装，见 §7），
> 再用 `rinf gen` 从带属性的 Rust 结构生成 Dart 侧类型。**不要手写桥接样板**。

**验收**：
- iPad 上出现完整着色的终端画面，`ls --color=always` / `git log --color` 颜色正确。
- CJK 按 2 列宽渲染、emoji 与零宽字符不错位。
- 旋转 / 改窗口 → `SessionUiCommand::Resize` → 远端 `WindowChange` → 远端 `stty size` 变化。
- `top` / `htop` 连续刷新下掉帧可接受；rinf 丢帧表现为**晚一帧**而不是花屏。
- **必须在这一步把帧率压到目标值**，不能留到 M4——这是整条路线唯一需要提前验证的技术风险。

**子项 M1-a：iPad 验证（**不需要真实 iPad 设备**）**

两条路，都不用买平板：

1. **"My Mac (Designed for iPad)"** —— 在 M1 上跑 **iPad 版**（不是 macOS 版）。
   用来做高频迭代：改一行看一眼，秒级反馈。
   注意目标名必须选 `Designed for iPad` 那个，**不能**选 My Mac 的 macOS 目标——
   后者是 macOS 二进制，跑通了也证明不了任何 iOS 的事。
2. **iPad 模拟器** —— 更接近真机的运行时环境（真实 iOS 系统库、真实 UIKit/IME 栈）。

验收标准按「iPad 版二进制在 iOS 运行时上跑通」来定，不按「macOS 上跑通」来定。
两者都不覆盖：jetsam 内存上限、真实触摸/IME 时序、App 审核沙箱限制——那些留给 M4 与真机。

**子项 M1-b（2026-09-22 缩减为小屏验收，并入 M2a）。** iPhone（6.1 寸）的连接、渲染、
输入体验已随 M2 实机验收；软键盘/IME 的专项验收本来就在 M2 清单内。剩下的只有
**小屏布局验收**：13 mini / 12 mini 到位后过一遍帧画布、键位条换行、安全区、旋转。

### M2 — 输入闭环 —— ✅ 已完成（2026-09-22 验收）

**前置：fork `terminal_view`（决策记录见 §6.1 末尾的 M2 fork 决定块）。**

- Flutter 的输入 → `TerminalInput::{CommittedText, Key{code, modifiers}}` →
  Rust `encode_input()`（alacritty 键编码，含 Kitty / CSI-u 协商）→ `transport.write()`。
- fork 改造（方案 A，结论见 §6.1 的 M2 fork 决定块）：
  render/widget 依赖的 `Terminal` 收窄成接口；painter/缓存零改动；
  App 侧写帧适配器（帧→BufferLine 行池，内容比对复用以保住行 Picture 重放）。
- 键位条：双排固定布局（修饰键挂住一次、长按锁定，Termux 同款；
  功能键即点即发）；用户自定义排布留给设置体系。

**验收**：iPad 上能跑通 `vim` 并保存；`Ctrl+C` 精确发一个 `ETX`；
中文 IME 的 **preedit 绝不外发**；`Ctrl`/`Esc`/`Tab`/方向键在软键盘上可达；
m1bar 60fps 不回退、M1 画面能力持平。
**范围排除**：滚动锁底部（scrollback 留 M4）；无选区/粘贴/鼠标（M2a）；
无字体/主题的用户自定义（M3+ 设置体系）。

### M2a — 指针交互（M2 与 M3 之间）—— ✅ 已完成（2026-09-25，模拟器验收；mini 真机复验待硬件）

选区（长按/拖动/按词）+ 复制/粘贴（bracketed paste 由引擎协商）+ 鼠标转发
（`encode_mouse`，htop 等程序的点击）。三者共享「帧模型上的选区/命中测试」地基——
这是 M1 勘察标的最大未验证块。验收：选词→复制到剪贴板；粘贴进 `vim`；
`htop` 里点击列头排序、点选进程。
另含 M1-b 缩减而来的**小屏验收**：13 mini / 12 mini 上过一遍布局
（帧画布、键位条换行、安全区、旋转）。
**交付**：选区与复制（含硬件键盘 ⌘C / ⌘A）全部走引擎；粘贴由 Rust 规范化并按远端模式包
bracketed paste；鼠标转发覆盖触摸点击、触摸拖动转滚轮、鼠标设备的按下/拖动/悬停与修饰键；
SGR 4 下划线绘制。决定见 §6.1「M2a 粘贴与鼠标决定」。

### M3 — 连接管理与凭证 —— ✅ 已完成（2026-09-25，模拟器验收；真机钥匙串与本地网络权限复验待硬件）

Flutter 表单做连接编辑，凭证进 iOS Keychain。
Rust 侧接口现成：`AuthPlan::from_profile(&profile, &vault)` + `CredentialVault`。

**验收**：连接增删改 + 密码进 Keychain，杀进程重启仍在；
主机密钥确认走真实 UI（`HostKeyPrompt{sha256, changed}`），
`changed=true` 时给显式警告而不是静默接受。

**交付**：连接目录（搜索、新建、编辑、复制、删除）与快速连接；密码可存钥匙串或每次询问
（询问时可勾选连接成功后保存）；首次连接确认主机密钥，密钥变更给出醒目警告、勾选核对后
才能替换（拒绝即中止）；keyboard-interactive 逐项问答；认证、网络、超时等失败分类提示，
局域网目标的网络类失败附本地网络权限说明与设置入口；设置页选字体与字号；会话信号带
`session_id`。决定见 §6.1「M3 连接与凭证决定」。

### M3a — 私钥认证与同步 —— ✅ 已完成（2026-09-25，模拟器验收；双设备同步实测待真机）

**已定（2026-09-22 拍板）**：私钥 blob 跨设备同步的安全取舍**接受**，但必须做成**开关**：
默认关闭；开关启用时明确告知「私钥将随 iCloud Keychain 上传同步」，用户同意后才写入
synchronizable 条目。（iOS 硬规则：`kSecClassKey` 不参与 iCloud Keychain 同步，
所以只能走 generic password 里的 blob。）

**交付**：私钥导入（粘贴或从「文件」选择；OpenSSH / PEM / PKCS#8 等格式，名称缺省用私钥
注释）、复制公钥、改名、删除（有连接在用时说明并拒绝）；连接可选私钥认证，私钥只在钥匙串
与内存里（rsHell 补丁 P4）；有口令的私钥连接时询问口令、输错重问，可存入钥匙串，也可再忘掉；
iCloud 钥匙串同步开关（默认关，开关前确认，私钥与口令整体搬迁）。
决定见 §6.1「M3a 私钥决定」。

### M3b — 硬件密钥：OpenPGP 卡 Ed25519 —— ✅ 已完成（2026-09-26，模拟器 + 模拟卡验收；真卡与 NFC 待真机）

**验证先行（不阻塞，不等 M3/M3a）**：

- ✅ **macOS 全链路已验通（2026-09-23）**：CanoKeys Canokey（认证槽 Ed25519）→
  `openpgp-card-ssh-agent` → 本机 russh 测试服务器，`ssh-ed25519` 签名认证成功。
  顺带验证两条事实：macOS 上 PC/SC 需要 `com.apple.security.smartcard`
  entitlement、GnuPG scdaemon 会独占锁卡
- 待验（iPhone 16，iOS 27）：USB-C 直插 + NFC 轻触两条通道（见「iOS 传输层」）

**前置：M3a 完成**。先有文件私钥认证再上外置卡——两者走同一套上游认证机制，
文件私钥先把路径验通（见 §5 M3a）。

**调研结论（2026-09-23，证据见 `docs/survey-m3b-2026-09-23.md`）**：

- 路线定案：**OpenPGP applet + ISO 7816 APDU**，不用 WebAuthn（→ M3d）、不用厂商
  SDK、不做 PIV。现有 CanoKey 的 Ed25519 只能走这条路
- 认证走**认证槽**：INTERNAL AUTHENTICATE（`00 88 00 00`）+ `VERIFY P2=82`
  （OpenPGP 卡规范 7.2.13.1 明说为 SSH 设计；gpg-agent 同款路径）；每次认证 =
  输 PIN + 按卡上按钮（CanoKey UIF 全 on）
- Ed25519 送**原始消息**（SSH 会话 blob），卡返回 64 字节裸签名 → `ssh-ed25519`
  直接可用，无 DigestInfo / mpint 转换
- ⚠ russh `Signer` 返回 `to_sign 原文 + string(算法名) + string(签名)`，
  **不是裸签名**（russh 源码 `client/encrypted.rs`；最容易做错）
- 上游 rsHell 原本**无 Signer 注入点**（`AuthPlan` 只有 Password/PublicKey/Agent/
  KeyboardInteractive）→ rsHell 补丁 P5 加 `ExternalSigner` 与 `AuthPlan::Signer`，
  卡签名器实现它。不走进程内 agent + `SSH_AUTH_SOCK`：上游 Agent 认证从进程环境变量
  找 agent，而进程已是多线程，设环境变量不安全；agent 协议里也没有会话，PIN 提问
  对应不到发起连接的会话
- PIN 交互：hub 自己的 `InteractionPrompt`（`CardPin`，带剩余次数），经会话的连接
  循环一问一答

**iOS 传输层**（复用 `card-backend` trait 自写 iOS 后端——iOS SDK 无
PCSC.framework，已确认）：

- 读卡器路线：`TKSmartCard`（iOS 9+）。**前提修正（2026-09-23）**：Apple 官方
  部署指南确认 iPhone/iPad 从 iOS 16 / iPadOS 16.1 起支持外置 CCID 读卡器
  （"plug in a smart card reader"，无需第三方驱动）；iPhone 15+（USB-C）在系统
  WebAuthn 流程支持 USB-C 安全密钥（Yubico 兼容表，2025-11 更新）。→ 现有
  CanoKey 直插 iPhone 16（USB-C）即可验
- NFC 路线：iOS 26+ `TKSmartCardSlotManager.createNFCSlot` + Info.plist
  `iso7816.select-identifiers`。⚠ YubiKey 的 OpenPGP/PIV 只在 USB 接触接口暴露
  （NFC 只给 FIDO2/OATH/OTP，官方原文待复核）——**YubiKey 5 NFC 走不了
  OpenPGP NFC**；用户的 CanoKey 是 USB+NFC 双接口，OpenPGP-over-NFC 待实测

**验收**：CanoKey（Ed25519）→ sshd 认证通过（先 macOS 后 iOS）；iOS 上读卡器 /
NFC 任一通道打通即可。

**范围排除**：RSA / ECDSA → M3c；WebAuthn / FIDO2 → M3d；PIV 不做。

**交付**：「私钥」页可以添加 OpenPGP 卡——读出插着的卡（iOS 26 起也可经 NFC），登记认证槽的
Ed25519 公钥（复制到服务器、改名、删除登记，与钥匙串私钥同一套管理）；连接选它即用卡登录：
卡在不在、密钥是否就是登记的那把先查，再问 PIN（显示剩余次数，错了重问，可记住到退出 App），
需要时提示按卡上的按键；卡不在、密钥不符、PIN 锁定、没按键等失败各有提示。
决定见 §6.1「M3b OpenPGP 卡决定」。

### M3c — OpenPGP 其他算法（RSA / ECDSA）—— ✅ 已完成（2026-09-26，模拟卡验收；真卡待硬件）

在 M3b 的卡认证上加两种算法，只是转换层：

- RSA：`authenticate_for_hash` 拼 DigestInfo（卡加 PKCS#1 填充）；签名算法跟着服务器走
  `rsa-sha2-256/512`，不支持 SHA-2 的旧服务器用 `ssh-rsa`（SHA-1，与钥匙串私钥一致）
- ECDSA（NIST P-256 / P-384 / P-521）：摘要算法由曲线定（RFC 5656），卡返回定长 r‖s →
  拆两半 → SSH mpint（个别卡每半多一个前导 0，mpint 编码时去掉）
- brainpool、secp256k1 等 SSH 不支持的曲线：读卡时标为不可用

**交付**：OpenPGP 卡的 RSA 与 ECDSA 认证密钥可以登记、登录；模拟卡加了 RSA 2048 与 P-256
（`GUOSH_VIRTUAL_CARD=rsa` / `p256`），测试里签名均用公钥验证。

### M3d — WebAuthn 安全密钥（sk-ecdsa）—— ⏸ 不急着做

- iOS `ASAuthorizationSecurityKeyPublicKeyCredentialProvider`（iOS 15+，系统 UI
  驱动 NFC/USB-C；Blink Shell 已商用）
- russh 依赖的 ssh-key 支持 sk-* 公钥（已实测）；sk 签名格式含 flags+counter，需自写
- ⚠ 风险：RP ID `"ssh:"` 能否被 iOS 接受（待实测）；服务端需 OpenSSH >8.2 且
  编译了 sk 支持（macOS 自带 sshd 没有）
- 与卡路线零共享（除 russh Signer 接缝）；估工独立 1~2 周。开启条件：M3b 绿后另立

### M4 — 产品化与合规

- 多标签 / 分屏（`PaneTree` / `SplitAxis` / `WorkspaceState` / `UiCommand::Split` 都已在 Rust 侧）
- **滚动回看的分级上界**：桌面契约是 `scrollback_lines` 上限 `1_000_000` 行，
  手机上会触发 jetsam 直接杀进程。必须另设平台分级上界。
- App 生命周期：进后台后 SSH 连接怎么处理（`SessionUiCommand::Reconnect` 已在协议里）
- 分发合规：App Review 2.5.2 对远程 shell 类应用有限制，定位与描述要提前想清楚。

### M5 — 平台宽度（Android / macOS，**非阻塞，但架构上现在就别堵死**）

**结论：能加，而且大部分是免费的**——因为已经定下的三条（Rust 是唯一权威、
rinf 单向信号流、Flutter 只画不解析）本身就与平台无关。

已经有的证据：

| 目标 | 状态 |
|---|---|
| macOS（`aarch64-apple-darwin`） | **已经在跑**：`m0_loopback` 就是 macOS 二进制，SSH 全链路通 |
| iOS / iPadOS | 编译 + 链接已验（§3.2） |
| Android | **未验**。`cargo check --target aarch64-linux-android` 只卡在 C 工具链：`failed to find tool "aarch64-linux-android-clang"`——即缺 NDK，**不是代码问题**。内核的平台层只分 `windows` / `unix`（`rshell-platform` 里没有 `target_os` 分支），Android 走 unix 分支 |

需要注意的（都不是拦路虎，是「别写反」）：

1. **不要在 Dart 层写 `Platform.isIOS` 分支。** 平台差异属于 Rust 侧
   （`rshell-platform` 已经是这个形状）。Dart 侧一旦开始判断平台，就说明有逻辑漏到前端了。
2. **「本地 shell 面板」按平台开关，而不是从协议里删掉。** iOS 上不暴露入口即可
   （`UiCommand::NewLocalTab` / `StartLocal` 都还在协议里）。macOS / Android 上
   `fork`+`openpty` 都在，**这两个平台可以白拿上游的本地面板能力**——那是产品的加分项。
   **这条直接决定了现在不要为了 iOS 去改上游删传输**（见 §9.2）。
3. **帧协议与度量回传保持平台中立。** `TerminalSize{cols,rows,pixel_width,pixel_height,dpi}`
   由前端量完回传，天然适用于任何平台。
4. **明文命名别带 `ios_` 前缀**，构建脚本按 `PLATFORM_NAME` 分派（`ios-host/build-rust.sh`
   已经是这个写法，加 Android 就是在同一个位置加分支）。

**先做哪一步的证据**：等 M1 在 iPad 上绿了，先只做一件事——
`cargo check --target aarch64-linux-android` 配上 NDK，把 Rust 侧的不确定量消掉。
Android 的 Flutter 侧产物在 M1 之后基本是免费的。

---

## 6. 复用清单（铁律 3 的落地）

### 6.1 已找到、优先复用

| 需求 | 候选 | 状态 | 备注 |
|---|---|---|---|
| **终端渲染层**（M1 核心） | [`terminal_view`](https://pub.dev/packages/terminal_view) | pub.dev v0.2.0（2026-09-02，Termphin），MIT，fork 自 xterm.dart 4.0.0 | **首选 fork 对象。** 移动端优先，changelog 明确写了「把相邻同风格单元格合并成一个 paragraph、相邻背景合并成一个 rect、把不再变化的行录成 Picture 重放、光标在 render object 里闪烁」——**正好就是 §4.3 测出来的那套优化**，而且已经实现了。目标就是「mid-range Android 上把忙碌的 `tail -f` 压到 60fps」 |
| 同上（备选） | [`lollipopkit/xterm.dart`](https://github.com/lollipopkit/xterm.dart) | MIT，活跃 | 有 Unicode 16 宽度表、grapheme cluster、**字形图集（glyph atlas）**、`CharMetricsCache` |
| 同上（备选） | [`dart_xterm`](https://github.com/cdrury526/dart_xterm) | MIT，标称 production-grade | xterm.dart 的另一个维护分支 |
| **mDNS / Bonjour 发现**（暂不实施） | [`nsd`](https://pub.dev/packages/nsd) | pub.dev 5.0.1 | iOS 13+，多平台，使用平台原生 API。**已决定 M0-c 不做自动发现，此条备查** |
| 同上（备选） | [`bonsoir`](https://pub.dev/packages/bonsoir) | pub.dev，iOS 13+ | 基于 `NWBrowser`，API 更现代（`eventStream`） |
| 同上（不推荐） | `flutter_nsd` | 原包与 AGP 8 不兼容，有第三方 fork | 生态较乱，优先上面两个 |

**⚠️ 复用 `terminal_view` / `xterm.dart` 的边界（对应铁律 4）：**
它们的 `Terminal` 是**完整的 VT100/xterm 解析器 + 缓冲区**。
我们**只复用它的绘制层**（`TerminalView` / render object / 字形缓存 /
选择手柄 / IME 接入 / 主题 / 鼠标点击与滚轮），
把数据源换成来自 Rust 的 `RenderFrame`。超链接、搜索、OSC 52 在它的绘制层里并不存在
（搜索测试整体注释掉、主题里的搜索色无人读取），需要时自己做。
**绝不使用它的解析器与缓冲区**——那会造出第二份终端状态权威。

**M1 第一遍的渲染决定（2026-09-15）：** M1 先用了一个 ~200 行的自写 `CustomPainter`
（`lib/src/terminal/terminal_painter.dart`）把 run 压缩帧画出来，理由：terminal_view 的
render object 与它自己的 `Terminal` 缓冲类型耦合，换数据源必须 fork，是独立的一块工程；
而 M1 的核心风险在「整条管线 + 帧率」，run 帧是纯展示数据、不含任何终端状态，自写
painter 不违反铁律 4。

**M2 fork 决定（2026-09-16，/grill-me 定案）：**
上游 `Termphin/terminal_view` @ v0.2.0，MIT；**GitHub fork + git 依赖**（与 Rust 上游
对称，源码不进本仓库）：fork 仓库 `Anthony-Hoo/terminal_view` 的 `guosh/frame-source` 分支。
开发期在仓库根放 `pubspec_overrides.yaml` 指向本地工作副本 `.forks/terminal_view`
（两者都不入库），改完推 fork、`pubspec.yaml` 换 ref。
接缝用**方案 A**：render/widget 依赖的 `Terminal` 收窄成接口，painter 与行/段落缓存
**零改动**，App 侧写帧适配器把解码后的帧**填进池化的真 BufferLine**（内容逐 run 比对、
相同则复用对象且不碰 `version`——行 Picture 重放的命中条件）；包内 parser/buffer
**留而不用**（不引用即不进 AOT 产物）。适配器放 App 侧（与 rinf 耦合属业务），
fork 保持通用。滚动锁底部，scrollback 留 M4；选区/粘贴/鼠标留 M2a。

**M2a 选区决定（2026-09-24）：**
选区权威在**引擎**（上游 `SelectionRange` 的坐标就是绝对行），Dart 不再自建锚点
——之前在 `TerminalController` 里重造的一套扛下三类 bug（键盘开合选区消失又出现、
拖左耳朵上移被清、拖过对端不换角色），全部废弃。链路：fork 手势（选词/拖选/手柄拖动）
→ `TerminalController.onSelectionIntent` 上报意图 → App 换算成 `stable_row` 发
`SelectionRequest` → 引擎持有选区、每次 `render(viewport, selection)` 都带上 →
回显 `SelectionState` → App 按当前帧把绝对行投影回视口，喂 fork 的纯坐标
`setExternalSelection`。帧的 stable→视口映射一变就重新投影（滚动/重排后高亮跟内容走）；
拖动进行中映射不变，乐观更新不会被迟到的回显顶掉。手柄用**官方控件**
（`material/cupertinoTextSelectionHandleControls.buildHandle`）画在 fork 的 widget 层
（`selection_handles.dart`），与高亮同一坐标来源；手柄是覆盖手势，点它不再触发
tap-down 清选区（所以官方的 translucent 在这里要换成 opaque）。复制走 `CopyRequest`
→ 引擎 `selected_text`（跨行拼行、裁尾空格都是引擎的职责）→ `ClipboardText` →
剪贴板 + 发清除。选区菜单按钮用官方 `ContextMenuButtonType.copy/paste`
（文案随 Flutter 本地化，接受英文）。fork 侧 API 变化随 fork 仓库自己的提交记录。

**文本输入决定（2026-09-25）：**
IME 连接背后的隐藏文本框每次提交后重置为初始值，但下一次按键可能在重置生效前到达平台。
fork 的 `CustomTextEdit` 因此改用 **delta 模型**（`enableDeltaModel`）：每条 delta 带着平台
自己的 `oldText`，按「组字区之前的已提交文本」前后差异算出「退格 n 次 + 输入一段文本」
（`text_input_delta.dart`），插入、删除、输入法整段/部分提交、替换都走同一条规则；预编辑
只绘制、绝不外发。智能引号与智能破折号关闭（终端要原字符）。
硬件键盘：无 Ctrl/Alt/Cmd 的可打印键交给平台文本输入（输入法才能组字，大小写与布局原样）；
回车、方向键、Ctrl 组合等直接处理的键不得越过之前敲下、仍在平台文本输入里的字符——
`HardwareKeyOrder` 把它们扣到平台答复了之前每个文本键（每键一条编辑更新）再放行，
平台不答复的键（死键等）150 ms 超时兜底。

**M3b OpenPGP 卡决定（2026-09-26）：**
卡上认证槽签名（`VERIFY P2=82` + INTERNAL AUTHENTICATE，Ed25519 直接签 SSH 数据），APDU 层用
openpgp-card，卡来源抽象为 `CardReader`：iOS / macOS 用 CryptoTokenKit（objc2 绑定，纯 Rust，
不写 Swift；读卡器插槽，iOS 26 起的 NFC 卡槽），debug 构建设 `GUOSH_VIRTUAL_CARD` 时另有一张
软件模拟卡（只实现认证用到的 APDU，release 里没有）。签名经上游 `ExternalSigner`（补丁 P5）接入：
服务器接受公钥后才动卡——先确认卡在、公钥与登记的一致、PIN 剩余次数，再问 PIN、让卡签名；
卡不在而设备能用 NFC 时先问 PIN 再弹 NFC 界面（系统界面会盖住 App）。PIN 错了立即丢掉记住的
PIN、带剩余次数重问；PIN 只在内存里记住（用户勾选时，到退出 App）。卡的登记与钥匙串私钥用同一种
信封（有卡号、没有私钥原文），连接配置同样写 `keychain:<id>`。同一时刻只有一个操作在用卡。

**M3a 私钥决定（2026-09-25）：**
私钥整块存钥匙串的 generic password（service `guosshell.ssh-key`，account 是私钥 id），内容是
JSON 信封：名称、私钥原文（可能加密）、OpenSSH 公钥、是否加密——同步到别的设备时信封完整；
列表靠按 service 搜索钥匙串得到，不另存元数据。口令单独一项（`guosshell.ssh-key-passphrase`），
与私钥放在同一个存储，只在解密成功后才写入。连接配置的 `identity_file` 写 `keychain:<id>`；
连接时 hub 从钥匙串读出、在内存里解密，经 `AuthPlan::from_private_key`（补丁 P4）认证，
私钥不落盘。导入时由 Rust 侧（russh 的 ssh-key）校验、算指纹，指纹相同的不重复导入；
OpenSSH 格式的加密私钥不用口令就能读出公钥，其他格式要口令。存储是 apple-native-keyring-store
的 protected data store：本机一个，`cloud-sync` 配置（synchronizable 条目）一个；synchronizable
条目不需要额外的 capability，是否真的同步取决于用户是否开启了 iCloud 钥匙串。同步开关存在
数据目录的 `preferences.json`；切换时私钥与口令先全部写进目标存储，有一项失败就撤回已写的，
全部成功才删原处；删除只动条目所在的存储。

**M3 连接与凭证决定（2026-09-25）：**
存储直接用上游：连接目录是 `rshell-storage` 的 SQLite（`SqliteRepository`），增删改一律走
`CredentialCoordinator::apply_catalog(mutation, SecretUpdate)`——目录与钥匙串之间的两阶段提交、
引用计数删除、崩溃后 `reconcile` 都是上游的；hub 不用 `ApplicationService`（它启动即开本地
shell）。数据目录由 Dart 用 path_provider 取 Application Support 交给 Rust（`AppStart`），
库与本 App 的 known_hosts 都在那里。iOS 上 keyring v1 不注册任何存储，启动时由
`rshell_m0::register_credential_store` 把 protected data store 设为默认（不随 iCloud 同步）。
密码每次连接从钥匙串读、不在内存缓存；没存或读不到就在连接前询问，勾选「保存」的在连接
成功后才写入（rsHell 补丁 P2 让 Password 认证可以不存密码）。主机密钥：新主机与变更的
密钥都问用户，变更时 `changed=true`，接受即替换该 host:port 的全部旧条目（补丁 P3）。
交互（密码、主机密钥、keyboard-interactive）经 `InteractionPrompt` / `InteractionReply`
一问一答；keyboard-interactive 中既无输入项也无说明的一轮直接回空答案。会话类信号都带
`session_id`（Dart 分配），hub 按它分发；连接期间的输入与尺寸变化连上后按顺序补上。
连接限时只算等网络的时间（60 秒），等用户回答（主机密钥、问答、PIN、按卡）时暂停——那段由服务器的
LoginGraceTime 把关，与 OpenSSH 客户端一致；交给传输层的连接上限（补丁 P6）与主机密钥确认的等待
都放宽到 30 分钟，只防一直没人回答。
失败分类（认证、主机密钥、网络、超时、钥匙串…）过边界，文案在 Dart；目标在局域网且
失败属网络或超时类时，Rust 给出系统设置 URL（iOS `app-settings:`），Dart 用 url_launcher 打开。
字体：内置 MesloLGS NF（常规与粗体，斜体由引擎合成），可选系统 Menlo；缺字回退内置字体。

**M2a 粘贴与鼠标决定（2026-09-25）：**
粘贴走 `PasteRequest`，由 Rust 处理：换行统一成 CR，剔除 Tab 以外的控制字符（防
`ESC[201~` 注入），远端开了 bracketed paste（DECSET 2004，模式由 rsHell fork 补丁 P1 暴露）
时包 `ESC[200~ … ESC[201~`；软键盘 / 输入法一次插入多行文本也改走粘贴。
鼠标：Dart 只转发，编码与模式判断全在 Rust（`encode_mouse`；远端没开对应的上报就丢弃，
同一格内的移动不重复上报）。触摸：点击 = 单击，拖动 = 滚动（全屏程序里转成滚轮，程序没开
鼠标上报时转成方向键），长按 = 本地选区。鼠标设备（iPad 触控板 / 鼠标、macOS）：按下、
拖动、松开、悬停全部上报并带 Shift / Alt / Ctrl；按住 Shift 走本地选区；已上报的按下不再
触发点按、双击选词、拖选或右键菜单。

### 6.2 明确不用

| 东西 | 为什么不用 |
|---|---|
| `dartssh2`（纯 Dart SSH） | 会把 SSH 搬到 Dart，直接违反铁律 1 |
| `xterm.dart` 的 `Terminal` 状态机 | 第二份状态权威，违反铁律 4 |
| `flutter_rust_bridge` | 已定用 rinf（统一信号规约，且单向信号流能防止业务滑向 Dart） |
| Rust 侧自渲染成位图 | 见 §4.3 第 6 条 |

### 6.3 还没找的（做之前必须先找）

- Flutter 侧的键位条 / 快捷键行（有大量现成实现，如终端类 App 的开源方案）
- 连接导入（`ssh_config` / 其他客户端迁移）——上游 `rshell-core::protocol::imports` 已有 Rust 侧逻辑

---

## 7. 环境要求

**本节只写「任何机器上都需要满足什么」。** 本机的版本号、工具绝对路径、真机清单、
签名配置等**一律不入库**。

| 项 | 要求 | 说明 |
|---|---|---|
| macOS | Apple Silicon | "My Mac (Designed for iPad)" 只在这类机器上可用 |
| Xcode | 16+ | §10 的坑清单都是对照 Xcode 16 记录的 |
| iOS SDK | 与 Xcode 配套 | `xcodebuild -showsdks` 能看到即可 |
| **iOS platform** | **必须单独下载** | **SDK ≠ platform。** 没装时 `-showdestinations` 报 `iOS x.y is not installed`，运行目标里一个 iOS 选项都没有。`xcodebuild -downloadPlatform iOS`（约 7–8 GB，含模拟器 runtime）。见 §5 M0b |
| Rust | 1.89+ | 需要 `aarch64-apple-ios` / `aarch64-apple-ios-sim` 两个目标 |
| `rinf` CLI | 8.x | M1 用 `rinf template` 铺 Flutter 骨架、`rinf gen` 生成 Dart 侧类型，**不手写桥接样板** |
| Flutter / Dart | 3.4x | M1 之后才需要 |
| Docker | 可选 | 本地验收 SSH 服务器 `scripts/sshd-test.sh`（vim / htop / 主机密钥变更等验收用） |

**工具不在 PATH 是常态**（cargo 常装在 `~/.cargo/bin`、Flutter 常由 fvm 之类的版本管理器
托管）。`scripts/setup.sh` 会主动探测并提示，不需要把路径写死在文档里。

---

## 8. 已定型的技术决定

| 决定 | 理由 |
|---|---|
| 桥用 **rinf**，不用 flutter_rust_bridge | 统一信号规约；单向信号流天然防止业务滑向 Dart。注意 rinf v8 已改用 serde + bincode（不再是 protobuf）；需要 protobuf 语义时用 `prost` 编码后塞进 `RustSignalBinary.binary` |
| 边界类型**沿用** `rshell-core::protocol` | 上游已有完整可序列化协议，重建等于在 Dart 写业务模型（违反铁律 1） |
| **不做本地 shell 面板** | iOS 不可能；Rust 侧零改动，Flutter 侧不暴露入口即可 |
| 帧格式：**结构化帧 + run 压缩 + 脏行增量** | §4.3 实测 |
| **帧流控：Dart 处理完回 `FrameAck`，同一时刻至多一帧在途** | rinf 的队列无界，逐块出帧会在高输出下积压；在途期间的变化只保留最新状态。两帧渲染起点至少相隔 8 ms（不合并 60 Hz 刷新），ACK 缺失 250 ms 后照发；会话结束前先画出待发帧 |
| 度量权威归 **Flutter** | 字体度量只有一个权威，就是实际画字的那一方。`TerminalSize{cols,rows,pixel_width,pixel_height,dpi}` 由 Flutter 测完回传 |
| 状态权威归 **Rust** | alacritty 网格只在 Rust 侧 |
| M0 宿主用 **Swift 而不是 Flutter** | 把「SSH 通不通」与「Flutter 构建集成」两个未知量分开；`.a` 在两种宿主下通用，M1 换宿主零返工 |
| M0 主机密钥策略：**TOFU 自动接受并落盘** | M0 前提是「不接 UI」，走完整确认会自相矛盾。**明确的技术债，M3 还清** |
| release profile **不要设 `panic = "abort"`** | 上游 actor 靠 `catch_unwind` 把 panic 转成 `SessionEvent::Crashed`（有 `actor_panic_gtk_survival_macos` 测试守着），abort 会毁掉这条韧性设计 |
| iPad 优先，iPhone 作为子项 | iPad 有大屏 + 硬件键盘 + 指针支持，能把最难的 IME/软键盘问题推到 M1-b |
| **仓库形态：独立仓库 + 上游经 fork 作 git 依赖** | 本仓库只装我们自己的代码。上游 pin 到 fork 上的具体 `rev`（基线 + 逐条记录的小补丁）；`Cargo.lock` 进版本控制保证复现。代价：首次构建要联网、升级上游要在 fork 里 rebase。见 `rust/UPSTREAM.md` |
| **keyring 的 iOS feature 从我们这边打开** | 不用改上游 `Cargo.toml`——Cargo 的 feature 是按包统一的。见 §3.2 |
| **不应用上游的 `portable-pty-psmux` patch** | 它的改动全在 `src/win/*`，我们的目标不编译这些文件；换 git 依赖后 dev-dep 的 feature 冲突也自动消失 |
| **iOS 上不删上游的本地传输，靠 `-Wl,-dead_strip` 裁符号** | 保留了 M5 里 macOS/Android 白拿本地面板的可能；将来真要摘掉就得 fork 上游（见 §9.2） |
| **链接可行性用命令行验，不用 Xcode** | `scripts/link-check.sh`：两个切片各链进一个 iOS 可执行文件 + 符号审计 |
| **连接方式：手工填写，不做自动发现** | 范围收窄，M0-c 只做「手填内网地址能连 + 权限处理 + 清晰失败提示」。mDNS/Bonjour 插件候选留在 §6.1 备查，不作为本期依赖 |
| **iPad 验证靠 "Designed for iPad" + 模拟器** | 不需要真实 iPad 设备。但**必须**是 iPad 版二进制跑在 iOS 运行时上，不能拿 macOS 目标冒充（见 §5 M1-a） |
| **平台宽度（Android / macOS）不阻塞，但架构上不许堵死** | 见 §5 M5。免费的部分（Rust 权威 + rinf + 纯 Flutter 绘制）现在就已经满足，要防的是「在 Dart 里写平台分支」和「为 iOS 删掉上游能力」这两件事 |

---

## 9. 待定项

### 9.1 已定（原待定项，2026-09-14 拍板）

| 原问题 | 决定 | 落地位置 |
|---|---|---|
| 仓库形态：fork 改名，还是新建仓库？ | **新建仓库**，上游源码 **vendor** 进 `rust/upstream/`，不与上游保持同步 | §8、`rust/upstream/PROVENANCE.md` |
| 内网子项要不要做 mDNS 自动发现？ | **不做**。只做手填内网地址 + 权限处理 + 失败提示 | §5 M0-c |
| iPad 设备怎么准备？ | **不需要真机**：先 "My Mac (Designed for iPad)"，再 iPad 模拟器，iPad 通了才做 iPhone | §5 M1-a / M1-b、§7 |

### 9.2 仍然开着（不阻塞 M0/M1，先记着）

1. **iPad 上的字号与列数默认值**：`TerminalSize` 由 Flutter 量完回传（§8），
   但 120×40 只是基准值。真实 iPad 上的默认字号/行列数要等 M1 画出来才好定。
2. **M3 的凭证 UI 形态**：已定——每次连接都读钥匙串，不在内存缓存（§6.1 M3 决定）。
   Face ID 保护（钥匙串条目的访问控制）留到需要时再加。
3. **上游 fork 的边界。** 已经 fork（UPSTREAM.md 的 P1 bracketed paste、P2 密码可不存、
   P3 主机密钥变更提示、P4 内存私钥认证、P5 外部签名认证、P6 连接单独限时）。还有两个已知的可能再改上游的需求，都不急：
   - **把 iOS 不可用的三个传输（`local` / `pty` / `system_ssh`）从编译图里摘掉**，
     而不是靠链接器裁符号。现在靠 `-Wl,-dead_strip` 能压到 0（§3.2），所以**不急**；
     但如果哪天想做 App Store 的静态审查友好度，或者要减 `.a` 的 65 MB，就得 fork 加 feature gate。
   - **给 `rshell-platform` 加真正的 iOS 分支**（它现在只分 `windows`/`unix`）。
     §2 里列过它「需要 iOS 分支」，但那可能是「实现时才发现不需要」——
     等到 M0-c（内网权限）或 M3（keyring）真的碰到壁垒再决定。
   **决策规则**：改上游就在 fork 的 `guosh` 分支加提交、换 `rev=`，
   在 `rust/UPSTREAM.md` 的「fork 补丁」里记下改了哪几行、为什么。
4. **自动发现（mDNS）要不要做**：本期明确不做，但如果 M0-c 的手填体验在局域网里太差，
   可以把它拉回来做一个独立里程碑（编号往后排，不要把 §5 的 M5 占掉——M5 是平台宽度）。
   候选插件已在 §6.1。
5. **Android 的 NDK 与 Flutter 侧**：见 §5 M5。等 M1 绿了再动，先消 Rust 侧的不确定量。

---

## 10. 陷阱清单

按踩坑概率排序。前三条是"不知道就会浪费一天"级别的。

1. **别用模拟器（或 "Designed for iPad"）验收 iOS 特有问题。** 它们跑在 macOS 用户态，
   `fork` / `openpty` 都在。拿它们测「本地 PTY 在 iOS 上不工作」会得出**错误结论**（会显示能跑）；
   反过来也会掩盖真正的沙箱问题。
   **但它们能验收「出站 TCP 在 iOS 沙箱里能不能连」**——这恰好是 M0b 要问的问题，
   因为这里唯一的平台差异就是 socket 与 App 沙箱。
2. **M0 的读循环必须有截止时间。** 真实远端 shell 既不 `Eof` 也不给 `ExitStatus`，
   会一直等下一条输入。只有 `exec` 模式（`ConnectionProfile::remote_command` 非空）
   才有退出码。「连上 → 读 → 等 Eof → 打印」这个最自然的写法在真机上永久挂住。
   （实测中我第一版就这么挂了。）
3. **内网连接的权限失败是静默超时，不是报错。** 见 §5 M0-c。
4. **`serde` 不开 `rc` feature 就序列化不了 `RenderFrame`。** 见 §2.2。
5. **`serde_json` 不能用于帧传输。** 700–850 KB/帧。见 §4.3。
6. **secret 类型不可序列化，协议层要手写转换。** 见 §2.2。
7. **不要在 release 里开 `panic = "abort"`。** 见 §8。
8. **锁屏/后台会挂起 SSH 连接。** 重连策略要做成显式产品行为，不能靠「不断开」蒙混。
9. **软键盘没有 `Ctrl`/`Esc`，`Cmd` 组合会被系统截获。** 能用的修饰键只有 `Ctrl`/`Alt`（和长按）。
10. **`rshell-platform::default_local_shell()` 在 iOS 上没有意义**（`SHELL` 环境变量不存在，
    兜底是 `/bin/sh`，而 iOS 上跑不了）。既然不做本地面板，调用点直接不暴露。
11. **Xcode 里 "My Mac" 有两个目标，选错等于白跑。** 名字都带 My Mac，但
    `My Mac (Designed for iPad)` 跑的是 **iPad 版二进制**，`My Mac` 跑的是 macOS 二进制。
    选后者再宣布「iPad 通了」是自欺——它连 UIKit 都没走。
12. **`xcodebuild -showsdks` 里有 iOS 18.2 ≠ iOS platform 装了。** Xcode 16 把平台支持
    拆出了 Xcode.app：**SDK 随 Xcode 装，但能被选中的 destination 要另外下**。
    没下之前 `-showdestinations` 报 `iOS 18.2 is not installed`，运行目标里
    **一个 iOS 选项都没有**。我之前混淆了这两者，在 M0b 上给出过
    「Designed for iPad 零下载」的**错误结论**。
13. **工程模板选 "Multiplatform App" 会堵死 "Designed for iPad"。** 那个模板生成
    `SDKROOT = auto` + `SUPPORTED_PLATFORMS = "iphoneos iphonesimulator macosx xros xrsimulator"`，
    一个 target 同时支持 iOS / macOS / visionOS，于是 Mac 上直接编成**原生 macOS App**。
    而「Designed for iPad」的前提正是 target **只**支持 iOS —— 两者互斥。
    改法：target → General → **Supported Destinations** → 删 `Mac` / `Apple Vision`，
    加 `Mac (Designed for iPad)`。
14. **Xcode 的 Run Script 跑在 bash 3.2 上。** 是 `/bin/bash` 3.2.57，
    不是 Homebrew 的 bash 5，也不是你的交互 shell。空数组展开 `"${arr[@]}"`
    （配合 `set -u`）报 `unbound variable`，`${var,,}` 报 `bad substitution` ——
    `ios-host/build-rust.sh` 一开始两条都犯了，Debug 构建必挂。
    往 Run Script 里加东西之前，先 `/bin/bash -n` 过一遍。
15. **Xcode 工程里 4 个手动配置点容易填错**，`ios-host/README.md` 里都标了 ⚠：
    ① bridging header 要写相对 `SRCROOT` 的**整条路径**（只写文件名 →
    `Build input file cannot be found`）；
    ② `Other Linker Flags` 不能把 `-lrshell_m0 -liconv` 存成一个带引号的字符串
    （→ `ld: library 'rshell_m0 -liconv' not found`），要拆成两个值；
    ③ 模板自带的 `GuoSSHellApp.swift` 必须删（它和 `M0Probe.swift` 各有 `@main`，
    且引用着已被删掉的 `ContentView`）；
    ④ Multiplatform 模板留下的 macOS entitlements（`com.apple.security.app-sandbox`）
    在 iOS 上没有意义。
16. **Xcode 的 User Script Sandboxing 会拦住脚本往构建目录写文件。** Xcode 15+ 新建工程默认
    `ENABLE_USER_SCRIPT_SANDBOXING = YES`，它生成的沙箱规则里显式
    `(deny file-read* file-write* (subpath (param "CONFIGURATION_BUILD_DIR")))` ——
    `CONFIGURATION_BUILD_DIR` 就是 `$(BUILT_PRODUCTS_DIR)`，而 `build-rust.sh` 正要往那儿
    `cp librshell_m0.a`。报错：
    `Sandbox: cp(1282) deny(1) file-write-create .../Debug-iphoneos/librshell_m0.a`。
    **修法**：给该 Run Script 声明 **Output Files** = `$(BUILT_PRODUCTS_DIR)/librshell_m0.a`。
    线索在沙箱文件末尾那句注释 `;; Allow read+write for declared and resolved (flattened)
    outputs` —— 没声明输出时它下面是空的；声明后 Xcode 会补上
    `(allow file-read* file-write* (literal (param "SCRIPT_OUTPUT_FILE_0")))`，
    `literal` 比 `subpath` 具体，覆盖那条 deny。
    详见 `ios-host/README.md` 坑 4。
17. **「本机能构建通过」不等于「沙箱放行」。** 自动化进程里 `sandbox-exec` 可能拿不到
    `sandbox_apply` 权限（报 `Operation not permitted`），此时脚本沙箱**根本没施加**，
    构建会「成功」得毫无意义。判断沙箱是否真生效，要去读 Xcode 写出的那份 `.sb`：
    `~/Library/Developer/Xcode/DerivedData/<Target>-*/Build/Intermediates.noindex/
    <Target>.build/<Config>-<platform>/<Target>.build/*.sb`。
18. **rinf 的 Rust 侧不要用 `current_thread` tokio 运行时。** `rinf template` 默认是
    `#[tokio::main(flavor = "current_thread")]`，实测 M1 的 SSH 连接在 TCP 建立后的
    握手/TOFU 交互回合**无限挂起**（同样代码在 M0 验证过的 `multi_thread` 上立即通过）。
    hub 的 main 已改为 `flavor = "multi_thread", worker_threads = 2`（与
    `rust/src/lib.rs` 的 `blocking_smoke` 一致）。症状识别：`connect().await` 不返回、
    对端却看到全部请求序列走完、无任何报错。
19. **rinf 8.10 的 `rinf template` 不会把 Dart 包 `rinf` 加进 pubspec**（只加 `meta` 和
    `tuple`），但模板的 `main.dart` 就在 import 它——不补 `flutter pub add rinf`
    会在 analyze/构建时报 `Undefined class 'RustSignalPack'` / 找不到 `package:rinf`。
    另外 v8.10 没有 `#[signal(binary)]` 属性：二进制信号用 `#[derive(RustSignalBinary)]`，
    字节作为 `send_signal_to_dart(binary)` 的**方法参数**传，不是字段。
20. **iOS 文本输入框的重置有竞态。** 提交后把隐藏文本框重置为初始值，下一次按键可能在重置
    生效前到达，平台给回的值里还带着上一段文本——按「初始值之后的全部」算增量就会重发
    （`echo` 变成 `eecechecho`，快速输入与输入法提交时稳定复现）。必须用 delta 模型，
    按平台自己的 `oldText` 计算。见 §6.1 文本输入决定。
21. **可打印键走平台文本输入是异步的，直接处理的键会越过它。** 硬件键盘上 `ls` + 回车打得快，
    会先执行 `l`、`s` 落到下一行。见 §6.1 的 `HardwareKeyOrder`。
22. **iOS 文本输入默认开着智能引号 / 智能破折号**（`TextInputConfiguration` 在非 obscureText 时
    默认 enabled），终端里 `"` 会变成 `“`，私钥里的 `-----` 会变成破折号、私钥就读不出来了。
    终端与所有技术字段（私钥、主机、命令、问答）都要显式关掉。
23. **模拟器的自动输入（`simctl` 等）走的是硬件键盘事件**，覆盖的是硬件键盘路径；软键盘路径
    （delta 插入/删除）要点屏幕键盘验证。模拟器默认输入法可能是拼音，英文输入先切到英文键盘。
24. **iOS 的「双空格变句号」**：在「词 + 空格」后再敲空格，iOS 把前一个空格替换成「.」
    （有时一步替换成「. 」）。终端里前一个空格早已发出，必须识别这条替换、按「用户敲的是
    空格」处理，且它不算对按键的答复（`text_input_delta.dart`）。
25. **`simctl pbcopy` 写进模拟器剪贴板的内容，App 读不到**（`Operation not authorized`）。
    粘贴验收先在 App 内复制；真机读其他 App 的剪贴板会弹授权提示（见 followup）。
26. **增删 `pubspec_overrides.yaml` 后要手动 `flutter pub get`**；`flutter run` 不重新解析依赖，
    会继续用 `pubspec.lock` 里的旧来源。
27. **keyring v1 在 iOS 上不注册任何凭证存储**（macOS / Windows / Linux 才自动注册），
    `Entry::new` 直接报 NoDefaultStore。启动时自己 `keyring_core::set_default_store`
    （`rshell_m0::register_credential_store`）。
28. **rinf 的两个方向语义不同**：Rust 收 Dart 信号有队列（先发后收不丢），但同一信号类型只有
    最后一个 `get_dart_signal_receiver()` 有效；Dart 收 Rust 信号是广播流，没人订阅时发来的就
    丢了——要先订阅再发请求（请求号配对的回答同理）。
29. **PAM 的 keyboard-interactive 在认证通过后还会发一轮「零输入项」请求**，要回空答案；
    弹给用户就是一个空对话框。
30. **对话框「自己关掉自己」要移除自己的 route，不能 `pop`**：它可能正在退场动画里，这时
    `Navigator.pop` 弹掉的是下面的页面（实测：拒绝主机密钥后终端页被一起关掉）。
31. **`PopScope(canPop: false)` 会直接禁用 iOS 的边缘右滑返回**，`onPopInvoked` 不会被调用；
    连接中的会话只能从页面上的「断开」离开。
32. **对话框里输入框的 `TextEditingController` 不能在 `showDialog` 返回后立刻 dispose**：
    对话框还在退场动画里、输入框仍在用它（debug 下断言 `_dependents.isEmpty` 红屏）。
    控制器交给对话框自己的 State 创建和销毁。
33. **上游 `NativeSshTransport` 的操作超时（默认 60 秒）覆盖整个 `connect`**，连用户看指纹、
    答问答、输 PIN、按卡的时间也算在内，`KnownHostsVerifier` 等主机密钥确认也另有 60 秒；
    慢一点就「连接超时」。见 §6.1 M3 决定（补丁 P6）。服务器那边还有 LoginGraceTime（常见
    120 秒），在登录界面停太久会被服务器断开。
34. **iPadOS 26 模拟器的窗口控件**：点到窗口左上角会展开红黄绿三个按钮、把 App 变成浮动窗口，
    之后的点击坐标全都错位；点绿色按钮回到全屏。

---

## 11. 可复现命令

```bash
# 环境准备（幂等；上游是 git 依赖，第一次需要联网）
./scripts/setup.sh

# 以下命令都在 rust/ 里跑
cd rust

# M0 端到端，不需要任何外部服务器或凭证
cargo run --example m0_loopback

# 打真实服务器
cargo run --example m0 -- <host> <port> <user> <password>

# 性能基准（本文档 §4 的全部数字）
cargo run --release --example bench_frame

# iOS 产物（静态库探针；M0b 壳退役后仅作编译冒烟）
cargo build --release --lib --target aarch64-apple-ios
cargo build --release --lib --target aarch64-apple-ios-sim
```

```bash
# 模拟器里用模拟 OpenPGP 卡（debug 构建）：带环境变量启动 App
SIMCTL_CHILD_GUOSH_VIRTUAL_CARD=1 xcrun simctl launch booted com.example.guoshShell

# 本地验收 SSH 服务器（Docker；probe / probe，127.0.0.1:2223，自带 vim / htop / m1bar）
./scripts/sshd-test.sh up
./scripts/sshd-test.sh rekey      # 重新生成主机密钥（验收密钥变更告警）

# 连它跑 App；exec 模式直接跑 60Hz×10s 进度条，debug 构建日志里看 [perf] 帧率
flutter run -d <模拟器id> \
  --dart-define=GUOSH_HOST=127.0.0.1 --dart-define=GUOSH_PORT=2223 \
  --dart-define=GUOSH_USER=probe --dart-define=GUOSH_PASS=probe \
  --dart-define=GUOSH_CMD=m1bar
```

（M0b 的「起飞前检查」`scripts/link-check.sh` 已随壳退役；App 构建由
flutter/Cargokit 全权负责。M0b 的 Xcode 工程建法在 git 历史的
`ios-host/README.md` 里可考。）

---

## 12. 当前进度

**已完成**

- [x] 可行性调研（alacritty 四层判定、rinf 事实纠正）
- [x] **M0a** 端到端（进程内环回，无外部依赖）
- [x] 帧传输性能基准（§4 的全部数字）
- [x] 复用候选调研（渲染层 `terminal_view`；mDNS 备查）
- [x] 全部产物收敛到 `GuoSSHell/`
- [x] **上游改为 git 依赖**，vendored 副本已删除（227 → 23 个文件）
- [x] **iOS 编译 + 链接实测**（§3.2）：两个切片都链得进 iOS 可执行文件，
      唯一额外标志 `-liconv`；PTY/fork 符号靠 `-Wl,-dead_strip` 归零
- [x] `scripts/link-check.sh`（不用开 Xcode 的起飞前检查）
- [x] `ios-host/build-rust.sh` 的 **bash 3.2** 兼容性修复
- [x] **M0b：`librshell_m0.a` 在真实 iOS App 里跑通** ——
      `My Mac (Designed for iPad)` 作为运行目标 + 内网 SSH 服务器 + 用户名密码认证通过。
      建这个工程踩的四个坑（iOS platform / Multiplatform 模板 / bash 3.2 / 脚本沙箱）
      全部记在 `ios-host/README.md`
- [x] **M0c：内网连接** —— `INFOPLIST_KEY_NSLocalNetworkUsageDescription` 已配，
      并在同一台内网服务器上验证通过。本期不做 mDNS 自动发现（§9）

**M0b/M0c 收尾与壳退役（2026-09-16）**

- [x] **M0b 壳退役**：删除仓库内 `ios-host/` 与 `scripts/link-check.sh`（M0 阶段的
      链接验证知识保留在 §10 与 git 历史）；仓库外的 Xcode 工程由用户自行删除
- [x] **Rust workspace 合并**：`rust/`（rshell-m0）并入根 workspace，
      `native/hub` 改为 path 依赖 + `rshell_m0` 再导出——上游 rev 只在
      `rust/Cargo.toml` pin 一处，`Cargo.lock` 只有根一个（rust/ 的已删）；
      iOS keyring `protected` feature 由 rust/ 侧 target 依赖经 unification 继续
      生效（`cargo check --target aarch64-apple-ios` + 完整模拟器构建 + 60fps
      冒烟实测通过）

**M1 —— 一帧终端画面 —— ✅ 已完成（2026-09-16，真机 60fps 满帧、0 丢帧）**

- [x] `flutter create` + `rinf template` 铺骨架（**先 iPad**）
- [x] M1 Rust 侧：`native/hub` 信号层（Connect/Resize/Disconnect + Status/FrameUpdate）、
      会话 actor（复用 M0 验证过的装配）、`pack_runs` 从 `bench_frame.rs` 提升进
      `frame_codec.rs`（`stable_row` 保持 i64 不截断；行级脏增量等上游暴露脏行信息再加）
- [x] M1 Dart 侧：帧解码 + run painter（§6.1 的 M1 决定）+ 连接表单 + 尺寸回传 +
      fps 计数器
- [x] **端到端实测（iOS 模拟器，iPad Pro 11-inch）**：`rust/examples/demo_server.rs`
      （无凭证环回服务器，probe/probe，密钥稳定）→ App 连上 → PTY-ACK →
      16 色 / truecolor / 反显 / 粗体 / CJK 2 列 / emoji 全部正确 →
      `RESIZE-ACK cols=98 rows=72`（旋转尺寸闭环）→ 流式日志滚动 → 光标块 →
      fps 计数器工作
- [x] **M1 验收全绿（2026-09-16，用户确认）**：真机内网实测 **60fps 满帧率**。
      实测工具链：exec 模式（连接表单「命令」栏 / `GUOSH_CMD`，上游 `configure_channel`
      的 `remote_command` 非空走 `channel.exec`，PTY 照开）+ `scripts/m1bar.sh`
      （60Hz×10s 进度条，600 帧绝对节拍）+ Rust 每 5s 发 `PerfStats`（render/pack
      avg·max、帧均字节）+ Dart 按 `FrameUpdate.seq` 跳变计丢帧。
      模拟器实测：600 帧 60.0Hz、**0 丢帧**、debug 构建单帧 render+pack ~2–5ms
      （预算 16.67ms，release 更低）。
- [x] terminal_view fork（字形缓存 / Picture 重放 / 选择 / IME）——随 M2 完成（b75d7bb）

**M2 —— 输入闭环 —— ✅ 已完成（2026-09-22）**

- [x] fork 方案 A + 帧适配器 + 双排键位条（b75d7bb）
- [x] 验收通过项：vim 编辑保存、IME preedit 不外发、m1bar 60fps 不回退、
      Ctrl+C 精确 ETX、软键盘 Enter 单发；iPhone（6.1 寸）实机连接与体验 OK
- [x] 实机连接修复：known_hosts 路径目录残留清理
      （followup「实机连接失败-Platform」已解决）
- [x] iOS 软键盘 Enter 双发：fork 血统 bug（iOS 上一次 Return 走
      performAction 与 "\n" 插入两条路），fork 侧去重修复，
      terminal_view ref 7f89795 → 21d04c2
- [x] M1-b 缩减为小屏布局验收，并入 M2a
- [x] 输入与帧回归修复（2026-09-25，模拟器实测）：软键盘大小写与数字、F 键、
      快速输入与输入法提交不再重发（delta 模型）、硬件键保序、关闭智能标点；
      重连不再卡「连接中」、连接中的尺寸变化连上后补发、pixel 为整个终端像素；
      密码所有权移入 SecretString；帧流控（ACK，至多一帧在途），m1bar 60fps 不回退、
      20 万行输出末屏完整；诊断探针移出连接路径

**M2a —— 指针交互 —— ✅ 已完成（2026-09-25，模拟器验收）**

- [x] 选区（长按选词 / 拖选 / 手柄）与复制走引擎，硬件键盘 ⌘C / ⌘A 同样走引擎
- [x] 粘贴：Rust 侧规范化 + bracketed paste（rsHell fork 补丁 P1）；vim autoindent 下
      多行粘贴不出台阶
- [x] 鼠标：htop 点列头排序、点选进程，触摸拖动转滚轮；鼠标设备的拖动 / 悬停 / 修饰键
      上报（fork 组件测试 + 引擎编码测试；iPad 触控板与 macOS 上实测随 M5）；中键修正
- [x] SGR 4 下划线绘制
- [x] 小屏布局：13 mini 模拟器竖屏；mini 真机复验待硬件

**M3 —— 连接管理与凭证 —— ✅ 已完成（2026-09-25，模拟器验收）**

- [x] 连接目录：新建、编辑、复制、删除、搜索；杀进程重启仍在；快速连接（debug 自动连接走它）
- [x] 钥匙串：保存的密码连接时读取；每次询问并可勾选保存；清除密码即删钥匙串条目；
      复制出的连接共用已存密码，删到最后一个才删条目
- [x] 主机密钥：首次确认指纹；容器 `sshd-test.sh rekey` 后给出变更警告，拒绝即中止、
      勾选核对后替换旧条目（rsHell 补丁 P3）
- [x] keyboard-interactive（容器开启 PAM 问答）；认证失败、网络失败、目标无效的分类提示；
      局域网目标的本地网络提示与设置入口
- [x] 设置：内置 MesloLGS NF / 系统 Menlo、字号；新会话生效
- [ ] 真机：钥匙串读写、本地网络权限弹窗与被拒后的提示（待硬件）

**M3a —— 私钥认证与同步 —— ✅ 已完成（2026-09-25，模拟器验收）**

- [x] 导入：粘贴与从「文件」选择；OpenSSH 加密私钥不用口令即可导入、PEM 加密私钥要口令；
      重复导入拒绝；名称缺省用私钥注释
- [x] 认证：Ed25519（无口令）与 RSA 3072（有口令）连容器；口令输错重问，存入钥匙串后
      重启 App 直接连上；忘掉口令后再次询问
- [x] 管理：复制公钥与原文件一致；改名；删除，有连接在用时说明并拒绝
- [x] 同步开关：开启确认后私钥与口令搬进 iCloud 钥匙串且照常连接，关闭后搬回本机
- [ ] 真机：双设备 iCloud 钥匙串同步（待签名构建与两台设备）

**M3b —— OpenPGP 卡 Ed25519 —— ✅ 已完成（2026-09-26，模拟器 + 模拟卡验收）**

- [x] rsHell 补丁 P5（`ExternalSigner`）：外部签名器连测试服务器认证通过；签名器失败即认证失败
- [x] 卡操作单测（模拟卡）：读卡、PIN 计数与锁定、签名可验、换卡 / 换密钥被拒、PIN 记住与取消
- [x] 模拟器（`GUOSH_VIRTUAL_CARD`）：添加卡、复制公钥授权到容器；连接时 PIN 错一次提示剩 2 次、
      输对后提示按键、容器按卡的 Ed25519 公钥放行；不带模拟卡启动时提示没有找到卡
- [ ] 真机：CanoKey 经 USB-C 直插 iPhone 登录容器；iOS 26 设备经 NFC 读卡
      （followup「OpenPGP卡NFC读卡待真机」）；macOS 随 M5

**M3c —— OpenPGP 卡 RSA / ECDSA —— ✅ 已完成（2026-09-26，模拟卡验收）**

- [x] 单测（模拟卡）：RSA 按服务器选 `rsa-sha2-256/512` 签名可验、旧服务器回落 `ssh-rsa`；
      P-256 签名可验；r / s 的前导 0 与最高位补 0
- [x] 模拟器：RSA 2048 与 P-256 模拟卡各自添加、授权到容器后登录成功；换了卡（密钥不同）时
      提示卡上的密钥与登记的不同
- [x] 连接限时：在 PIN 框停过 60 秒不再「连接超时」（rsHell 补丁 P6 + 等用户时暂停计时）
- [ ] 真机：RSA / ECDSA 的 OpenPGP 卡实测（待硬件）
- [ ] M3d / M4 / M5

**关于提交**

main 上的提交需要签名，而签名在自动化环境里做不到（私钥不在磁盘上，必须人工操作）。
因此自动化在**功能分支**上提交（不签名；按里程碑一条分支，依次叠在前一条上），人工审阅后
重签合入 main。需要直接在 main 上提交时，自动化只负责 `git add` 和把提交信息写成文件，
最后一步由人工执行：

```bash
git commit -F <提交信息文件>
```

---

## 13. M0b 的 Xcode 壳怎么办（M1 迁移说明）

**结论：保留在本地，不删、不提交。**

它是 M0b 的一次性宿主，M1 换成 Flutter 之后就没有独立价值了。但它现在是唯一能把
「Rust 侧坏了」和「Flutter 侧坏了」分开的东西，建议留到 **M1 绿了**再删。

它**本来就不在本仓库里**（Xcode 工程是单独建的），所以「不提交」不需要额外做什么。

### 到 M1 时，配置怎么迁移

Flutter 生成的 `ios/Runner.xcodeproj` 和手建的壳**结构不同**，别指望照搬。逐项对照：

| M0b 壳里的东西 | M1（Flutter + rinf）下怎么办 |
|---|---|
| Run Script 调 `build-rust.sh` | **这个机制要保留** —— Xcode 默认仍会开 User Script Sandboxing、仍用 bash 3.2，两个坑一个不少。脚本内容换成 rinf 的构建流程（`rinf template` 生成的骨架已经带好） |
| Run Script 的 **Output Files** 声明 | **同样要加**，否则 `cp` 被沙箱拦（§10.16） |
| `LIBRARY_SEARCH_PATHS` / `OTHER_LDFLAGS = -lrshell_m0 -liconv` | **`-liconv` 大概率仍需要**（rusqlite bundled SQLite 与 ring 的依赖不变）；链接方式改用 rinf 的（Cargokit / `rinf.framework`），由 Podfile 或 xcconfig 管 |
| `SWIFT_OBJC_BRIDGING_HEADER` + C 头 | **不再需要。** rinf 走 Dart FFI 生成的绑定，不是 Swift 直接调 C —— 这是两套架构最大的差异 |
| `DEAD_CODE_STRIPPING = YES` | **保持 YES**（Xcode 默认就是），PTY/fork 符号归零靠它 |
| `INFOPLIST_KEY_NSLocalNetworkUsageDescription`（M0c 的成果） | **要搬。** M1 的内网连接同样需要它，否则权限失败表现为静默超时 |
| `GuoSSHell.entitlements`（macOS `app-sandbox`） | 丢掉。那是 Multiplatform 模板的残留，iOS 上没意义 |
| 壳自己的 `.git` | 不需要。M1 的产物进主仓库 |

### 一句话

**能迁移的是「配置清单与坑」，不是「工程文件」。** 那些坑已经全部写在
`ios-host/README.md` 和本文档 §10 里 —— 即使壳哪天丢了，照着这两份文档重新点一遍
也能复现。**所以不必为了「保存壳」而把它提交进仓库。**
