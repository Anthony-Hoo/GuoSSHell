//! 私钥（存在钥匙串里；私钥本身永不过边界，导入时的原文除外）。

use rinf::{DartSignal, RustSignal, SignalPiece};
use serde::{Deserialize, Serialize};

/// 列表里的一把私钥。
#[derive(Serialize, SignalPiece)]
pub struct KeySummary {
    pub id: String,
    pub name: String,
    /// 算法（`ssh-ed25519`、`ssh-rsa`、`ecdsa-sha2-nistp256` …）。
    pub algorithm: String,
    /// SHA256 指纹。
    pub fingerprint: String,
    /// OpenSSH 一行格式的公钥（复制到服务器的 authorized_keys）。
    pub public_key: String,
    /// 私钥有口令保护。
    pub encrypted: bool,
    /// 口令已存进钥匙串。
    pub passphrase_saved: bool,
    /// 在 iCloud 钥匙串里（随 iCloud 同步）。
    pub synchronized: bool,
    /// 使用它的连接数。
    pub used_by: u32,
}

/// 私钥列表与同步开关。私钥或开关变化后重发。
#[derive(Serialize, RustSignal)]
pub struct KeyListState {
    pub keys: Vec<KeySummary>,
    /// 私钥经 iCloud 钥匙串同步（新导入的私钥也放进 iCloud 钥匙串）。
    pub sync_enabled: bool,
}

#[derive(Deserialize, DartSignal)]
pub struct KeyQuery {}

/// 导入私钥。`private_key` 是私钥原文（OpenSSH / PEM / PKCS#8 / PuTTY 格式）；
/// `passphrase` 可为空——OpenSSH 格式的加密私钥不需要口令也能导入，其他格式要。
#[derive(Deserialize, DartSignal)]
pub struct ImportKey {
    pub request_id: u32,
    pub name: String,
    pub private_key: String,
    pub passphrase: String,
}

#[derive(Deserialize, DartSignal)]
pub struct RenameKey {
    pub request_id: u32,
    pub id: String,
    pub name: String,
}

/// 删除私钥（连同存下的口令）。有连接在用时拒绝。
#[derive(Deserialize, DartSignal)]
pub struct DeleteKey {
    pub request_id: u32,
    pub id: String,
}

/// 删掉存下的口令，下次连接时再问。
#[derive(Deserialize, DartSignal)]
pub struct ForgetPassphrase {
    pub request_id: u32,
    pub id: String,
}

/// 开关 iCloud 钥匙串同步：已有的私钥与口令随之移入或移出 iCloud 钥匙串。
#[derive(Deserialize, DartSignal)]
pub struct SetKeySync {
    pub request_id: u32,
    pub enabled: bool,
}

#[derive(Serialize, SignalPiece, Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    None,
    /// 不是能识别的私钥。
    Invalid,
    /// 私钥加密了，这种格式要口令才能读出公钥。
    PassphraseRequired,
    PassphraseWrong,
    /// 同一把私钥已经导入过。
    AlreadyExists,
    NotFound,
    /// 还有连接在用。
    InUse,
    Keychain,
    /// 写不进 iCloud 钥匙串。
    SyncUnavailable,
}

/// 私钥操作的结果，`request_id` 原样带回；`key_id` 是导入的私钥。
#[derive(Serialize, RustSignal)]
pub struct KeyResult {
    pub request_id: u32,
    pub error: KeyError,
    pub key_id: String,
}
