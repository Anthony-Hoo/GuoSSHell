//! `hub` —— rinf 的 Rust 侧入口。
//!
//! * `signals`       rinf 边界上的信号类型（适配层，不重建上游业务模型）
//! * `app`           启动：数据目录 → 存储（连接目录 + 钥匙串）→ 各处理任务
//! * `catalog`       连接目录的查询与增删改
//! * `settings`      设置（字体、字号）
//! * `keys`          私钥（钥匙串）：导入、列表、iCloud 同步
//! * `connect`       建立连接：认证材料、主机密钥与交互问答
//! * `session`       会话 actor：引擎、帧、输入
//! * `frame_codec`   RenderFrame → run 压缩字节流（自 bench_frame.rs 提升）
//! * `local_network` 本地网络权限的失败提示

mod app;
mod catalog;
mod connect;
mod frame_codec;
mod keys;
mod local_network;
mod session;
mod settings;
mod signals;

use rinf::{dart_shutdown, write_interface};
use tokio::spawn;

write_interface!();

// multi_thread 是 M0 验证过的形态（rust/src/lib.rs 的 blocking_smoke 用
// worker_threads=2）。current_thread 下 TOFU 交互回合会饿死（实测：
// TCP 连上后握手无限挂起；换 multi_thread 立即通过）。
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    // 等 Dart 给出数据目录后打开存储，再起目录、设置与会话的处理任务。
    spawn(app::run());

    dart_shutdown().await;
}
