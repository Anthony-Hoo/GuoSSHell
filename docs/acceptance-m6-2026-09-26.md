# M6 验收计划：coding agent、全屏 TUI 与设备矩阵（2026-09-26）

> M6 的验收定义：要回答的问题、测试台、用例、度量与通过标准、真机清单，以及执行结果。
> PLAN.md §5 M6 只放摘要，细节以本文件为准。执行结果回填到 §8。

## 1. 要回答的问题

1. **coding agent**：Claude Code、Codex CLI、opencode 在 GuoSSHell 里运行时——模型高速流式输出、
   连续工具调用、多个 subagent 并发且在它们之间切换视图——App 有没有**性能问题**（帧率下降、显示
   落后于输出、打字不跟手、内存上涨）和**显示 bug**（字符丢失或错位、残影、闪烁、滚回被刷乱、画面卡住）。
2. **全屏 TUI**：btop、nvtop、网速监控（nload、bmon、iftop）进入全屏（备用屏）、全屏中改尺寸、
   退出全屏时有没有问题。
3. **设备**：iPhone 17 Pro Max、iPhone 17、iPad Pro 13 英寸、iPad Pro 11 英寸上运行正常；
   iPad 上硬件键盘与鼠标 / 触控板事件正常。

## 2. 测试台

### 2.1 验收服务器

沿用 `scripts/sshd-test.sh`（Docker，登录 probe / probe），镜像扩展为 M6 全套，基础镜像换成
Debian trixie（btop 1.3.2、nvtop 3.2.0 在 main 里）：

| 内容 | 说明 |
|---|---|
| 三个 agent | Claude Code、Codex CLI、opencode，版本 pin 在 Dockerfile。配置预置在 probe 的家目录：全部指向容器内的假上游；跳过登录、引导、信任目录与权限确认；关闭自动更新、遥测与后台下载 |
| 假 AI 上游 | 容器内常驻（§2.2），只听容器内回环；请求日志另经一个端口映射到本机回环，供测试断言 |
| 全屏 TUI | btop、nvtop（配假 GPU，§2.4）、nload、bmon、iftop（`cap_net_raw`，非 root 可用）、htop、vim |
| 持续流量 | 容器内 iperf3 在回环上持续收发，网速监控有数可看 |
| 辅助程序 | `/usr/local/bin/m6-*`（§2.3） |

容器随 `sshd-test.sh up` 起 sshd、假上游、流量与日志服务；日志服务只读地提供假上游的请求日志与
回显程序记下的输入（本机回环端口）。

### 2.2 假 AI 上游

**复用 [aimock](https://github.com/CopilotKit/aimock)（MIT，Node，无依赖）**，版本 pin 在 Dockerfile。
它同时实现三种协议的流式应答，含工具调用与思考块：

| agent | 协议 |
|---|---|
| Claude Code | Anthropic Messages（SSE） |
| Codex CLI | OpenAI Responses（SSE；Codex 已不支持 Chat Completions） |
| opencode | OpenAI Chat Completions（SSE，经 `@ai-sdk/openai-compatible`） |

我们只写一层剧本（Node 脚本，用 aimock 的编程接口：请求 → 按剧本生成回复）：

- **选场景**：用户第一句话里的标记 `[m6:<场景>]`；subagent 由派发时写进它 prompt 的标记
  `[m6-sub:<场景>:<序号>]` 识别。
- **定轮次**：剧本发出的每个工具调用的 id 都带「场景 / 轮次」，下一次请求里最近的工具结果指向哪一轮，
  就回下一轮——回复只由请求内容决定，并发与重试都安全，不需要会话状态。
- **工具意图按 agent 翻译**：执行命令、读文件、写文件、改文件、派 subagent、更新待办，映射到各 agent
  自己的工具名与参数（请求里带的工具列表为准）。
- **速率**：按剧本分档（每档是 aimock 的每秒块数 × 每块字符数），首 token 延迟可配。
- **后台 subagent**：Claude Code 的 subagent 在后台运行——派发后立刻得到「已启动」的工具结果，
  每个完成时再以一条用户消息通知主 agent；主 agent 的剧本按收到的完成通知数决定何时总结。
- **旁路请求**：agent 自己发的会话标题、摘要等请求（不带工具），回短文本或它要的 JSON，不进剧本。
- **确定性**：文本由固定种子生成，同一场景每次输出相同，截图与统计可以前后对比。

剧本（每个场景的最后一条消息以 `M6-DONE <场景>` 结尾，供自动化判断结束）：

| 场景 | 主 agent | subagent |
|---|---|---|
| `stream` | 一轮：思考约 300 token + 正文约 6000 token（标题、列表、代码块、表格、中英混排），约 2000 token/s | — |
| `burst` | 同上，约 8000 token/s | — |
| `tools` | 6 轮：执行命令、读文件、写文件、改文件（diff）、上千行输出的命令、再执行；每轮前后各有一段流式文字；最后总结 | — |
| `subagents` | 一轮派 6 个 subagent → 等结果 → 约 1500 token 总结（约 3000 token/s） | 各自 2 轮工具调用 + 约 1200 token 总结（约 2000 token/s） |
| `long` | 40 轮，每轮约 400 token + 一次输出 200 行的命令 | — |
| `cjk` | 中文为主的 markdown（长段落自动折行、表格、代码注释） | — |

emoji 只放在 Claude Code 与 opencode 的剧本里：aimock 按 UTF-16 分块会把 emoji 切成两半，
Codex 的解析器会丢掉那一块。

### 2.3 辅助程序（容器内）

| 命令 | 作用 |
|---|---|
| `m6-agent <claude\|codex\|opencode> <场景>` | 在一个干净的工作目录里以场景标记为开场白启动 agent 的交互界面 |
| `m6-tui <程序>` | 先打印标记行，再启动 TUI；退出后打印标记行并查询终端模式（见 `m6-probe`） |
| `m6-probe` | 用 DECRQM / CPR / DA 查询终端当前的模式与光标（TUI 退出后查残留） |
| `m6-keyecho [模式…]` | 原始模式下按要求打开鼠标上报 / bracketed paste / 应用光标键，记录收到的每个字节（屏幕 + 日志） |
| `m6-sync <情形>` | 同步输出（DEC 2026）：成对、BSU 后停住、BSU 后被杀 |
| `m6-cjk` | 宽字符排布图样：整行中文、宽字符后的着色段、emoji、组合字符、框线 |
| `m6-flood <速率>` | 可控速率的彩色 / 中文洪水输出 |

回显程序只记录原始字节，断言直接对照期望的字节序列（xterm 规范），不在容器里做解码。

### 2.4 nvtop 的假 GPU

nvtop 没有 GPU 时打印「No GPU to monitor.」直接退出。**复用 NVIDIA 的
[mocknvml](https://github.com/NVIDIA/k8s-test-infra/tree/main/pkg/gpu/mocknvml)**（Apache-2.0）：
假的 `libnvidia-ml.so`，Dockerfile 里多阶段构建（Go + cgo），GPU 数量与型号由它的 YAML 配置给出；
不配置进程列表（配了会让 nvtop 崩溃）。nvtop 的显示逻辑与真实 GPU 相同。

### 2.5 App 侧度量与自检

| 项 | 来源 | 说明 |
|---|---|---|
| 出帧率、render / pack 耗时、帧字节 | Rust `PerfStats`（已有，5 秒一窗） | — |
| **显示延迟** | Rust `PerfStats`（新增） | 远端字节进引擎 → 含它的帧被 Dart 确认，p50 / p95 / max；帧流控下 Dart 跟不上会表现为它变大 |
| ACK 超时次数、远端输出吞吐 | Rust `PerfStats`（新增） | ACK 超时 = Dart 250 ms 内没处理完一帧 |
| 帧应用耗时 | Dart | 解码 + 填行池 |
| Flutter 帧 build / raster、卡顿帧 | Dart（`FrameTiming`） | 卡顿 = 超过一个刷新周期 |
| 内存 | Dart | 进程 RSS |
| **画面一致性** | Rust + Dart（新增，测试用） | 引擎按列给出当前屏幕，与 Dart 行池逐列比对——字符丢失、错位、最后一帧没画上都会被抓到 |

度量一窗一行 JSON 打进日志（`[m6-perf] {…}`），debug 与 profile 构建都有；集成测试直接订阅，写进报告。

### 2.6 自动化

| 层 | 工具 | 覆盖 |
|---|---|---|
| App 内 | Flutter `integration_test`（`flutter drive`；iOS 模拟器 + macOS） | 连验收服务器，跑 A、B、C、E 的场景；读 App 自己的终端缓冲断言；画面一致性；截图；度量汇总成 JSON |
| 系统输入 | XCUITest（`ios/RunnerUITests`，iPad 模拟器） | 系统合成的硬件键盘（`typeKey` + 修饰键）与指针事件（悬停、点按、右键、拖动、滚动），走 UIKit → Flutter 引擎的真实路径（D 类） |
| 编排 | `scripts/m6.sh` | 起服务器、建 / 起四台模拟器、按矩阵跑、汇总报告 |

模拟器上 Flutter 只有 debug 构建（profile / release 不支持模拟器），所以**性能门槛在 macOS 的
profile 构建与真机上判**；模拟器只看功能与相对值。

## 3. 用例

### A. coding agent

三个 agent（A1 Claude Code、A2 Codex CLI、A3 opencode）各跑下列场景。

| 场景 | 做什么 | 重点看 |
|---|---|---|
| S1 高速流式 | `stream`、`burst`；流式中在 agent 输入框打字 | 帧率、显示延迟、打字回显延迟、最终画面完整且一致 |
| S2 工具调用 | `tools` | diff 着色与对齐、长输出的折叠与滚动、每轮切换时的重绘 |
| S3 多 subagent 并发与切换 | `subagents`；进行中切到子视图、在子视图间切换、回到主视图（Claude Code ↓ 选中后台 agent、Enter 查看、Ctrl+O 详细记录；Codex Alt+← / →；opencode Ctrl+X ↓ / ← / → / ↑） | 并发刷新下的帧率与延迟、切换视图时的整屏重绘、无残影 |
| S4 长会话 | `long` | 内存、滚回上翻、选区复制 |
| S5 打断与退出 | 流式中 Esc / Ctrl-C 打断；正常退出；流式中直接杀掉 agent | 回到 shell 后画面与终端模式干净（同步输出、鼠标上报、键盘协议、光标） |
| S6 改尺寸 | 流式中旋转、iPad 分屏窗口变化、软键盘升降 | 重排正确、无错位残留 |
| S7 中文 | `cjk` | 折行后每行完整、宽字符后的着色段位置正确、表格对齐 |

三个 agent 默认都是全屏界面（备用屏 + 鼠标上报）；另用**行内模式**（Claude Code 的经典渲染、
Codex 的 `--no-alt-screen`、opencode 的 `--mini`）跑 S1 与 S7——这条路径走主屏与滚回，Codex 还会用
滚动区域把历史插到视口上方。

另测**多窗格**：一个窗格跑 `burst`，另一个窗格的 shell 里打字，回显延迟不受影响。

### B. 全屏 TUI

程序：btop（`update_ms=100`）、nvtop（100 ms 一刷）、nload、bmon、iftop；htop、vim 作回归。

| 编号 | 检查 |
|---|---|
| B1 进入 | 进入备用屏、铺满窗格（行列与窗格一致，最后一行不被键位条或 Home 指示条挡住）、边框对齐、颜色正确；滚回不增长 |
| B2 运行 | 高刷新下的帧率与显示延迟；触摸拖动 → 滚轮或方向键；鼠标点击（btop 点进程、切面板） |
| B3 改尺寸 | 旋转、iPad 分屏窗口变化后重排正确，无旧画面残留；小于程序最小尺寸时（iPhone 竖屏的 btop）显示它自己的提示，放大后恢复 |
| B4 退出 | 回到进入前的画面（之前打印的标记行还在，提示符紧随其后）、光标可见、备用屏与鼠标上报已关、滚回里没有 TUI 画面、触摸拖动重新滚动滚回 |
| B5 异常退出 | SIGTERM / SIGKILL 后远端不复位时的表现与其他终端一致，`reset` 能恢复；SSH 断开（冻结容器）后重连，终端模式由 App 复位，新的 shell 不在备用屏里、不收鼠标上报 |

### C. 设备矩阵

iOS 27 模拟器：iPhone 17 Pro Max、iPhone 17、iPad Pro 13 英寸（M5）、iPad Pro 11 英寸（M5）。
每台：构建、安装、启动、连上；竖屏与横屏布局（安全区、灵动岛、Home 指示条、键位条、标签条）；
A 的 S1 + S3（三个 agent）、B 全套；关键画面截图归档。

### D. iPad 键盘与鼠标

XCUITest 合成事件，远端 `m6-keyecho` 核对收到的字节：

| 编号 | 事件 | 期望 |
|---|---|---|
| D1 | 字母、数字、符号（含 Shift 大小写）、空格 | 原字符 |
| D2 | 回车、退格、Tab、Shift+Tab、Esc | CR、DEL、HT、`CSI Z`、ESC |
| D3 | 方向键、Home、End、PgUp、PgDn、Delete、F1–F12 | 标准序列；远端开了应用光标键时方向键为 `SS3` |
| D4 | Ctrl+字母、Ctrl+[ ] \ 等 | C0 控制字符 |
| D5 | Option+字母、Option+方向键 | ESC 前缀；`CSI 1;3x` |
| D6 | ⌘T / ⌘W / ⌘D / ⌘C / ⌘V / ⌘A / ⌘1…9 | App 自己处理，不发到远端 |
| D7 | 一次连续输入 50 个键再回车 | 顺序不乱、不丢不重 |
| D8 | 鼠标左键单击、双击、右键 | 远端开了鼠标上报时收到按下 / 松开；没开时走本地（选词、菜单） |
| D9 | 按住拖动 | 1002 下收到拖动；没开上报时是本地选区 |
| D10 | 悬停移动 | 1003 下收到移动，同一格不重复 |
| D11 | 滚轮 / 触控板双指滚动 | 全屏程序里是滚轮事件（或方向键），shell 里滚动滚回 |
| D12 | Shift / Option / Control + 点击或滚动 | 修饰键位正确；Shift+点击走本地选区 |

另外在 agent 里验实际用法：opencode 里点击与滚动、Codex 与 opencode 里鼠标滚动 transcript、
多行输入（Shift+Enter 或各 agent 的换行键）、Esc 打断、Ctrl-C。

### E. 终端协议（agent 与 TUI 依赖的部分）

| 编号 | 项 | 谁在用 |
|---|---|---|
| E1 | 同步输出（DEC 2026）：BSU / ESU 成对时整帧出现；BSU 之后 150 ms 内没有 ESU 也要照常显示 | Codex、opencode、btop 每帧都用 |
| E2 | 查询应答：DA1 / DA2、XTVERSION、CPR、DECRQM、OSC 10 / 11、kitty 键盘查询，按提问顺序作答；未知的 OSC / DCS / APC 静默吞掉 | 三个 agent 启动时都成批查询（Claude Code 以 DA1 应答为界，之前没答的当作不支持） |
| E3 | kitty 键盘协议（影响 Shift+Enter 等组合键） | Codex 不查询直接推送；opencode 查询后才推送 |
| E4 | 焦点上报 1004、bracketed paste 2004、OSC 52 复制、OSC 8 链接、频繁改标题（Codex 每 100 ms）、BEL / OSC 9 通知、REP（`CSI b`）、DEC 线框字符 | 各 agent 与 TUI |

## 4. 度量与通过标准

**性能**（macOS profile 构建代表本机；真机你来验，§7）：

| 指标 | 门槛 |
|---|---|
| 显示延迟（S1、S3、B2 期间） | p95 ≤ 50 ms，max ≤ 250 ms |
| ACK 超时 | 0 |
| 出帧率 | 不低于远端的刷新节奏（上限按屏幕刷新率） |
| Flutter 卡顿帧 | ≤ 1% |
| 打字回显延迟 | shell 里 p95 ≤ 50 ms；agent 输入框 p95 ≤ 150 ms（含 agent 自己的节流） |
| 多窗格 | 另一窗格高速输出时，本窗格回显延迟 p95 仍 ≤ 50 ms |
| 内存（S4） | RSS 增长不超过滚回上界对应的量 + 50 MB，结束后不再上涨 |

**模拟器**（debug 构建）：不设绝对门槛，但显示延迟不得随时间持续增长（积压），不得出现超过 1 秒
的画面停滞。

**显示**：场景结束时画面一致性通过（引擎与 Dart 逐列相同）、画面含 `M6-DONE <场景>`、无 U+FFFD、
无泄漏到屏幕上的转义序列文本；截图人工复核无错位、残影，颜色与边框正确。

**TUI**：B1–B5 的断言全部通过。**键盘鼠标**：D1–D12 每个事件远端收到的字节与期望一致。

## 5. 立项时已确认的问题

立项调研时在模拟器与引擎上已经复现的问题，M6 当场修，修后由上面的用例守住：

| # | 问题 | 复现 | 影响 | 处理 |
|---|---|---|---|---|
| 1 | 帧编码按「格」而不是「列」记 run 的位置与长度，Dart 再按自己的简化宽度表重排 | 整行中文只画出前一半；`中文` 后接着色的 `X` 显示成 `中X` | 所有含中文 / emoji 的输出，agent 的中文回答尤甚 | 编码按列给出位置并带上每格宽度，Dart 不再自己算宽度（宽度的权威在引擎，铁律 4） |
| 2 | 同步输出没有超时：BSU 之后没有 ESU，后面的输出（包括 shell 提示符）都不再显示 | `printf '\e[?2026h'` 后画面卡死 | Codex、opencode、btop 每帧都用；它们在一帧中途被杀（Ctrl-C、崩溃、SSH 断开）就会卡死 | rsHell 补丁：引擎给出同步截止时刻，会话循环到点结束同步（与 alacritty 一致，150 ms） |
| 3 | 硬件键盘快速连续输入时，回车越过前面还在输入法通道里的字符 | 模拟器一次键入 `…cjk.sh⏎`，先执行了 `…cjk.s` | 快速打字、文本扩展、扫码枪 | 保序等待的超时按「最后一次进展」计，而不是按最后一次按键 |

另有几项需要结合 agent 实测再定（结论写进 §8，属于产品决定的开 followup）：

- XTVERSION（`CSI > q`）不应答：Claude Code 因此不查 2026、不用同步输出，一帧可能被拆成两次显示；
- OSC 10 / 11（前景 / 背景色查询）一律答黑色：Codex 与 opencode 用它选深浅配色与输入框底色；
- kitty 键盘协议默认关闭：Shift+Enter 与 Enter 分不开；
- OSC 52 被忽略：opencode 的选中即复制、Codex 的复制进不了系统剪贴板；
- 焦点上报（1004）从不发送：Codex 靠它决定通知与自动摘要。

## 6. 范围排除与限制

- 真机：本机没有设备，§7 列给你验；
- 模拟器只能跑 debug 构建，性能门槛在 macOS profile 与真机上判；
- agent 自身的 bug 不修，只记录终端这一侧的问题；
- nvtop 用假 NVML，网速监控只看容器内回环流量；
- 假上游不模拟真实模型的行为，只模拟输出的节奏与形态。

## 7. 真机清单（你来验）

1. **性能**（profile 构建，`flutter run --profile`）：iPhone 与 iPad 上各跑三个 agent 的 `burst` 与
   `subagents`、btop，看日志里的 `[m6-perf]`；ProMotion 设备看出帧能否到 120 Hz；
2. **iPad + 妙控键盘 / 触控板**：D1–D12 手动走一遍，重点是触控板的惯性滚动、台前调度窗口缩放、
   外接键盘的 Esc / Ctrl；
3. **iPhone 软键盘**：键位条的 Esc / Ctrl / 方向键在三个 agent 里的实际使用；
4. **进后台与锁屏**：agent 流式中切走再回来、锁屏再解锁，会话与画面状态。

## 8. 结果

（执行后回填）

## 9. 调研依据

调研于 2026-09-26，均来自源码（对应 tag）与官方文档：

- **假上游**：比较了 aimock、llm-mock-server、MockServer、llm-d-inference-sim、VidaiMock、mockllm 等。
  只有 aimock 同时具备三种协议的流式工具调用、按请求内容匹配、可编程回复、速率控制与多架构运行，
  MIT。它的缺口都能在配置层绕开：Responses 不输出工具命名空间 → Codex 开 `multi_agent_v2`；
  不支持 Codex 的 WebSocket 续接 → 自定义 provider 本来就不用 WebSocket；emoji 分块 → 见 §2.2。
- **Claude Code 2.1.274**：npm 包只是启动器，实际是按平台分发的 Bun 编译二进制（linux-arm64 为 glibc 版）。
  用 `ANTHROPIC_AUTH_TOKEN` + `ANTHROPIC_BASE_URL` 时没有连通性检查与 API key 确认；预置
  `~/.claude.json`（已完成引导、工作目录已信任）与 `~/.claude/settings.json`（权限模式）即直接进入输入框；
  关闭非必要流量后完全离线可用。每轮 `POST /v1/messages?beta=true` 流式；首条消息另发一条不带工具的
  标题请求。subagent 工具名 `Agent`，交互模式下在后台并发运行；界面上 ↓ 进入后台 agent 列表、Enter
  查看某个 agent。默认全屏（备用屏 + 1000/1002/1003/1006 鼠标上报），另有经典的行内渲染；启动时按
  XTVERSION、`CSI ?u`、DA1 的顺序查询，XTVERSION 有应答才用 DECRQM 查 2026、支持即每帧同步输出；
  帧间隔至少 16 ms；标题每 960 ms 转圈；复制走 OSC 52。
- **Codex CLI 0.157.1**：只支持 Responses；默认备用屏 + 全部鼠标上报；每帧同步输出；启动时成批查询
  CPR、OSC 10 / 11、`CSI ?u`、DA1 / DA2（共 250 ms）；不查询直接推 kitty 键盘标志；标题每 100 ms 转圈；
  subagent 用 `/subagents` 或 Alt+← / →（输入框为空时）切换；会发不带工具的标题请求；
  需要模型目录文件避免每轮警告；自定义 provider 不用 WebSocket。
- **opencode 1.18.32**：Chat Completions 经 `@ai-sdk/openai-compatible`（编进二进制，不在运行时下载）；
  备用屏 + 1003 全程鼠标上报；每帧同步输出；启动时查询 OSC 10 / 11、XTVERSION、CPR、DECRQM、`CSI ?u` 等；
  收到 `CSI ?u` 应答才推 kitty 键盘标志；选中即经 OSC 52 复制；subagent 用 Ctrl+X 引导键切换；
  标题生成用 small model。
- **TUI**：trixie 的 btop 1.3.2 总是真彩色、每帧同步输出、鼠标常开、最小 80×24；nload / bmon 无需权限，
  iftop 需要 `cap_net_raw`；多数 TUI 在 SIGHUP / SIGKILL 下不复位终端（SSH 断开即 SIGHUP）；
  nvtop 用 DEC 线框字符，nload 用 REP。
