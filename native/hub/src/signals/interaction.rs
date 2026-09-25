//! 连接过程中需要用户回答的问题：密码、主机密钥确认、keyboard-interactive。
//!
//! 主机密钥与 keyboard-interactive 来自上游的 `InteractionBroker`；密码是 hub 自己问的
//! （连接没存密码，或钥匙串里读不到）。一次只问一个，Dart 回答后才会有下一个。

use rinf::{DartSignal, RustSignal, SignalPiece};
use serde::{Deserialize, Serialize};

#[derive(Serialize, SignalPiece, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Password,
    HostKey,
    KeyboardInteractive,
}

/// keyboard-interactive 的一个输入项。
#[derive(Serialize, SignalPiece)]
pub struct PromptField {
    pub label: String,
    /// 输入是否可以明文显示（服务器指定；密码类为 false）。
    pub echo: bool,
}

/// 发给 Dart 的问题。按 `kind` 读对应字段，其余为空。
#[derive(Serialize, RustSignal)]
pub struct InteractionPrompt {
    pub session_id: u32,
    pub prompt_id: u32,
    pub kind: PromptKind,
    /// 连接目标（所有种类都填）。
    pub username: String,
    pub host: String,
    pub port: u16,
    /// HostKey：实际连上的地址（known_hosts 按它记录；主机名解析后的 IP）。
    pub address: String,
    /// HostKey：密钥算法与 SHA256 指纹。
    pub algorithm: String,
    pub fingerprint: String,
    /// HostKey：与记录的密钥不一致（可能是中间人攻击）。
    pub changed: bool,
    /// KeyboardInteractive：服务器给的标题、说明与输入项。
    pub name: String,
    pub instruction: String,
    pub fields: Vec<PromptField>,
    /// Password：回答可以存进钥匙串（目录里的连接才可以，快速连接不行）。
    pub can_remember: bool,
}

/// 对 `prompt_id` 的回答。`accept = false` 是取消 / 拒绝。
///
/// `answers`：Password 是一项（密码），KeyboardInteractive 与输入项一一对应，
/// HostKey 为空。答案按明文过边界，Rust 收到即包成 `SecretString`。
#[derive(Deserialize, DartSignal)]
pub struct InteractionReply {
    pub session_id: u32,
    pub prompt_id: u32,
    pub accept: bool,
    pub answers: Vec<String>,
    /// Password：连接成功后把这个密码存进钥匙串（仅目录里的连接）。
    pub remember: bool,
}
