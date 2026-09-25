//! OpenPGP 卡（M3b）：用卡上**认证槽**的密钥做 SSH 公钥认证——VERIFY PW1（P2=82）后
//! INTERNAL AUTHENTICATE，Ed25519 直接签 SSH 要签的数据。私钥从不离开卡。
//!
//! APDU 层是 openpgp-card；卡从哪里来由 [`CardReader`] 决定：CryptoTokenKit（iOS /
//! macOS 的读卡器与 NFC 卡槽）、测试与 debug 构建里的模拟卡。签名经上游的
//! `ExternalSigner`（rsHell 补丁 P5）接进认证，见 [`signer`]。

#[cfg(any(target_os = "ios", target_os = "macos"))]
mod ctk;
mod signer;
#[cfg(any(test, debug_assertions))]
pub mod virtual_card;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use card_backend::CardBackend;
use openpgp_card::Card;
use openpgp_card::ocard::algorithm::{AlgorithmAttributes, Curve};
use openpgp_card::ocard::crypto::{EccType, PublicKeyMaterial};
use openpgp_card::ocard::{KeyType, StatusBytes};
use openpgp_card::state::{Open, Transaction};
use rshell_m0::russh::keys::ssh_key::public::{Ed25519PublicKey, KeyData};
use rshell_m0::russh::keys::{HashAlg, PublicKey};
use secrecy::SecretString;

pub use signer::{CardRequest, CardSigner, PinQuestion};

/// 能提供卡的地方。
pub trait CardReader: Send + Sync {
    /// 现在能用的卡（每个读卡器上一张），尚未打开。
    fn cards(&self) -> Vec<Box<dyn CardBackend + Send + Sync>>;

    /// 这台设备能用 NFC 读卡。
    fn nfc_supported(&self) -> bool {
        false
    }

    /// 弹出系统的 NFC 读卡界面，等卡靠近；返回的会话在 drop 时结束、界面收起。
    /// 会话存续期间，靠近的卡出现在 [`Self::cards`] 里。
    fn begin_nfc(&self, _message: &str) -> Result<Box<dyn NfcSession>, CardFailure> {
        Err(CardFailure::NotFound)
    }
}

/// 一次 NFC 读卡（系统界面）。drop 即结束。
pub trait NfcSession: Send {}

/// 没有卡（测试）。
#[cfg(test)]
pub struct NoCards;

#[cfg(test)]
impl CardReader for NoCards {
    fn cards(&self) -> Vec<Box<dyn CardBackend + Send + Sync>> {
        Vec::new()
    }
}

/// 几个来源合在一起（真实读卡器 + debug 的模拟卡）；NFC 交给第一个支持的。
pub struct Readers(pub Vec<Arc<dyn CardReader>>);

impl CardReader for Readers {
    fn cards(&self) -> Vec<Box<dyn CardBackend + Send + Sync>> {
        self.0.iter().flat_map(|reader| reader.cards()).collect()
    }

    fn nfc_supported(&self) -> bool {
        self.0.iter().any(|reader| reader.nfc_supported())
    }

    fn begin_nfc(&self, message: &str) -> Result<Box<dyn NfcSession>, CardFailure> {
        self.0
            .iter()
            .find(|reader| reader.nfc_supported())
            .ok_or(CardFailure::NotFound)?
            .begin_nfc(message)
    }
}

/// 本平台的卡来源。debug 构建里设了 `GUOSH_VIRTUAL_CARD` 时再加一张模拟卡。
pub fn platform_reader() -> Arc<dyn CardReader> {
    let mut readers: Vec<Arc<dyn CardReader>> = Vec::new();
    #[cfg(any(target_os = "ios", target_os = "macos"))]
    readers.push(Arc::new(ctk::CtkReader));
    #[cfg(debug_assertions)]
    if std::env::var_os("GUOSH_VIRTUAL_CARD").is_some() {
        readers.push(Arc::new(virtual_card::VirtualCard::for_debug()));
    }
    Arc::new(Readers(readers))
}

/// 卡操作失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardFailure {
    /// 没有找到这张卡（没插、没靠近，或 NFC 读卡被取消）。
    NotFound,
    /// 卡上认证槽的密钥与登记时的不同。
    KeyMismatch,
    /// 认证槽没有密钥，或算法暂不支持（M3b 只做 Ed25519）。
    Unsupported,
    PinWrong { tries_left: u8 },
    PinBlocked,
    /// 卡要求按键确认，但没有等到。
    TouchTimeout,
    /// 用户取消了 PIN 输入。
    Cancelled,
    /// 其他读卡错误。
    Io(String),
}

impl From<openpgp_card::Error> for CardFailure {
    fn from(error: openpgp_card::Error) -> Self {
        match error {
            openpgp_card::Error::CardStatus(StatusBytes::AuthenticationMethodBlocked) => {
                Self::PinBlocked
            }
            openpgp_card::Error::CardStatus(StatusBytes::SecurityRelatedIssues) => {
                Self::TouchTimeout
            }
            error => Self::Io(error.to_string()),
        }
    }
}

/// 读卡得到的认证槽信息。
#[derive(Debug, Clone)]
pub struct CardInfo {
    /// 卡号（`厂商:序列号`，openpgp-card 的 ident）。
    pub ident: String,
    pub cardholder: String,
    /// 认证槽的算法（界面显示用）。
    pub algorithm: String,
    /// 认证槽的公钥；`None` = 没有密钥，或算法暂不支持。
    pub public_key: Option<PublicKey>,
    /// PIN 还能试几次。
    pub pin_tries_left: u8,
    /// 签名要在卡上按键确认。
    pub touch: bool,
}

/// 进程内的卡状态：卡来源、上次读卡的结果（登记时用）、用户选择记住的 PIN。
pub struct CardContext {
    pub reader: Arc<dyn CardReader>,
    scanned: Mutex<Vec<CardInfo>>,
    pins: Mutex<HashMap<String, SecretString>>,
    /// 同一时刻只有一个操作在用卡（读卡器会话是独占的）。
    busy: Mutex<()>,
}

impl CardContext {
    pub fn new(reader: Arc<dyn CardReader>) -> Self {
        Self {
            reader,
            scanned: Mutex::new(Vec::new()),
            pins: Mutex::new(HashMap::new()),
            busy: Mutex::new(()),
        }
    }

    /// 读一遍现在能用的卡（`nfc` 时先弹 NFC 界面等卡）。结果留着，登记时用。
    pub fn scan(&self, nfc: bool) -> Result<Vec<CardInfo>, CardFailure> {
        let _busy = lock(&self.busy);
        let _nfc = if nfc {
            Some(self.reader.begin_nfc("将 OpenPGP 卡靠近设备")?)
        } else {
            None
        };
        let cards: Vec<CardInfo> = self
            .reader
            .cards()
            .into_iter()
            .filter_map(|backend| {
                match Card::new(backend)
                    .map_err(CardFailure::from)
                    .and_then(|mut card| read_info(&mut card))
                {
                    Ok(info) => Some(info),
                    Err(error) => {
                        rinf::debug_print!("[card] skipping a card: {error:?}");
                        None
                    }
                }
            })
            .collect();
        *lock(&self.scanned) = cards.clone();
        Ok(cards)
    }

    /// 上次读卡看到的这张卡。
    pub fn scanned(&self, ident: &str) -> Option<CardInfo> {
        lock(&self.scanned)
            .iter()
            .find(|card| card.ident == ident)
            .cloned()
    }

    fn remembered_pin(&self, ident: &str) -> Option<SecretString> {
        lock(&self.pins).get(ident).cloned()
    }

    fn remember_pin(&self, ident: &str, pin: SecretString) {
        lock(&self.pins).insert(ident.to_owned(), pin);
    }

    /// PIN 错了或卡锁了：立刻丢掉记住的 PIN，免得再拿它去试、把卡试锁。
    fn forget_pin(&self, ident: &str) {
        lock(&self.pins).remove(ident);
    }

    /// 连接前的检查：卡在不在、认证槽的公钥是否就是登记的那把、PIN 还剩几次。
    fn probe(&self, ident: &str, expected: &PublicKey) -> Result<CardInfo, CardFailure> {
        let _busy = lock(&self.busy);
        let info = read_info(&mut self.open(ident)?)?;
        check_key(&info, expected)?;
        Ok(info)
    }

    /// 验证 PIN 后让卡签名，返回 SSH 签名 blob。`nfc` 时先弹 NFC 界面等卡靠近（整个过程
    /// 一次靠近完成）。`before_sign` 在送签名命令前调用（提示用户按卡上的按键）。
    #[allow(clippy::too_many_arguments)]
    fn sign(
        &self,
        ident: &str,
        expected: &PublicKey,
        pin: SecretString,
        data: &[u8],
        hash: Option<HashAlg>,
        nfc: bool,
        before_sign: &dyn Fn(bool),
    ) -> Result<Vec<u8>, CardFailure> {
        let _busy = lock(&self.busy);
        let _nfc = if nfc {
            Some(self.reader.begin_nfc("将 OpenPGP 卡靠近设备以完成登录")?)
        } else {
            None
        };
        let mut card = self.open(ident)?;
        let info = read_info(&mut card)?;
        check_key(&info, expected)?;
        if hash.is_some() {
            return Err(CardFailure::Unsupported);
        }
        let mut tx = card.transaction()?;
        if let Err(error) = tx.verify_user_pin(pin) {
            return Err(pin_failure(&mut tx, error));
        }
        before_sign(info.touch);
        let signature = tx.card().internal_authenticate(data.to_vec())?;
        if signature.len() != 64 {
            return Err(CardFailure::Io(format!(
                "unexpected Ed25519 signature length {}",
                signature.len()
            )));
        }
        Ok(ssh_signature("ssh-ed25519", &signature))
    }

    /// 找到卡号为 `ident` 的卡（已选中 OpenPGP 应用）。
    fn open(&self, ident: &str) -> Result<Card<Open>, CardFailure> {
        for backend in self.reader.cards() {
            let mut card = match Card::new(backend) {
                Ok(card) => card,
                Err(error) => {
                    rinf::debug_print!("[card] not an OpenPGP card: {error}");
                    continue;
                }
            };
            let found = card
                .transaction()
                .and_then(|tx| tx.application_identifier())
                .is_ok_and(|aid| aid.ident() == ident);
            if found {
                return Ok(card);
            }
        }
        Err(CardFailure::NotFound)
    }
}

/// 认证槽的公钥得是登记的那把（M3b 只认 Ed25519）。
fn check_key(info: &CardInfo, expected: &PublicKey) -> Result<(), CardFailure> {
    match &info.public_key {
        Some(key) if key.key_data() == expected.key_data() => Ok(()),
        Some(_) => Err(CardFailure::KeyMismatch),
        None => Err(CardFailure::Unsupported),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

fn read_info(card: &mut Card<Open>) -> Result<CardInfo, CardFailure> {
    let mut tx = card.transaction()?;
    let ident = tx.application_identifier()?.ident();
    let cardholder = tx.cardholder_name().unwrap_or_default();
    let algorithm = tx.algorithm_attributes(KeyType::Authentication)?;
    let public_key = if is_ed25519(&algorithm) {
        tx.public_key_material(KeyType::Authentication)
            .ok()
            .and_then(|material| ed25519_public_key(&material, &ident))
    } else {
        None
    };
    let pin_tries_left = tx.pw_status_bytes()?.err_count_pw1();
    let touch = tx
        .user_interaction_flag(KeyType::Authentication)?
        .is_some_and(|uif| uif.touch_policy().touch_required());
    Ok(CardInfo {
        ident,
        cardholder,
        algorithm: algorithm_name(&algorithm),
        public_key,
        pin_tries_left,
        touch,
    })
}

/// VERIFY 失败 → 失败原因。卡对错 PIN 回 63Cx（x = 剩余次数），有的卡（YubiKey）回 6982，
/// 那就再读一次 PIN 状态。
fn pin_failure(tx: &mut Card<Transaction<'_>>, error: openpgp_card::Error) -> CardFailure {
    let tries_left = match error {
        openpgp_card::Error::CardStatus(StatusBytes::PasswordNotChecked(tries_left)) => tries_left,
        openpgp_card::Error::CardStatus(StatusBytes::SecurityStatusNotSatisfied) => {
            match tx.invalidate_cache().and_then(|()| tx.pw_status_bytes()) {
                Ok(status) => status.err_count_pw1(),
                Err(error) => return error.into(),
            }
        }
        error => return error.into(),
    };
    if tries_left == 0 {
        CardFailure::PinBlocked
    } else {
        CardFailure::PinWrong { tries_left }
    }
}

fn is_ed25519(algorithm: &AlgorithmAttributes) -> bool {
    matches!(algorithm, AlgorithmAttributes::Ecc(ecc)
        if ecc.ecc_type() == EccType::EdDSA && *ecc.curve() == Curve::Ed25519)
}

fn algorithm_name(algorithm: &AlgorithmAttributes) -> String {
    match algorithm {
        AlgorithmAttributes::Rsa(rsa) => format!("RSA {}", rsa.len_n()),
        AlgorithmAttributes::Ecc(ecc) => format!("{:?}", ecc.curve()),
        AlgorithmAttributes::Unknown(_) => "unknown".to_owned(),
    }
}

/// 卡给的 Ed25519 公钥（32 字节，个别卡带 0x40 前缀）→ OpenSSH 公钥，注释写卡号。
fn ed25519_public_key(material: &PublicKeyMaterial, ident: &str) -> Option<PublicKey> {
    let PublicKeyMaterial::E(ecc) = material else {
        return None;
    };
    let point = match ecc.data() {
        [0x40, rest @ ..] if rest.len() == 32 => rest,
        point => point,
    };
    let point: [u8; 32] = point.try_into().ok()?;
    Some(PublicKey::new(
        KeyData::Ed25519(Ed25519PublicKey(point)),
        format!("cardno:{}", ident.replace(':', "")),
    ))
}

/// SSH 签名 blob：`string(算法名) || string(签名)`。
fn ssh_signature(algorithm: &str, signature: &[u8]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(8 + algorithm.len() + signature.len());
    for part in [algorithm.as_bytes(), signature] {
        blob.extend_from_slice(&u32::try_from(part.len()).unwrap_or(u32::MAX).to_be_bytes());
        blob.extend_from_slice(part);
    }
    blob
}

#[cfg(test)]
mod tests;
