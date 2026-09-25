//! 软件模拟的 OpenPGP 卡：只实现 SSH 认证用到的几条 APDU——SELECT、GET DATA（应用数据、
//! 持卡人）、读认证槽公钥、VERIFY、INTERNAL AUTHENTICATE。
//!
//! 测试用；debug 构建里设了环境变量 `GUOSH_VIRTUAL_CARD` 时它也作为一个读卡器出现，
//! 用来在没有卡、没有真机的环境（模拟器）里走通卡认证的整条界面。release 构建里没有它。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use card_backend::{CardBackend, CardCaps, CardTransaction, PinType, SmartcardError};
use rshell_m0::russh::keys::ssh_key::private::Ed25519Keypair;
#[cfg(test)]
use rshell_m0::russh::keys::ssh_key::PublicKey;
use rshell_m0::russh::keys::ssh_key::PrivateKey;
use rshell_m0::russh::keys::signature::Signer as _;

use super::CardReader;

/// 模拟卡的用户 PIN（OpenPGP 卡的出厂默认值）。
pub const PIN: &str = "123456";
/// 模拟卡的卡号：厂商号 FFFF 是规范留给测试的。
#[cfg(test)]
pub const IDENT: &str = "FFFF:00000001";
/// 界面与日志里的持卡人名。
pub const CARDHOLDER: &str = "GuoSSHell Virtual Card";

const OPENPGP_AID: [u8; 6] = [0xD2, 0x76, 0x00, 0x01, 0x24, 0x01];
/// AID：RID + 应用 01 + 版本 3.4 + 厂商 FFFF + 序列号 00000001 + RFU。
const AID: [u8; 16] = [
    0xD2, 0x76, 0x00, 0x01, 0x24, 0x01, 0x03, 0x04, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
];
/// Ed25519（EdDSA 0x16 + OID 1.3.6.1.4.1.11591.15.1）。
const ED25519: [u8; 10] = [0x16, 0x2B, 0x06, 0x01, 0x04, 0x01, 0xDA, 0x47, 0x0F, 0x01];
/// X25519（ECDH 0x12 + OID 1.3.6.1.4.1.3029.1.5.1）。
const X25519: [u8; 11] = [
    0x12, 0x2B, 0x06, 0x01, 0x04, 0x01, 0x97, 0x55, 0x01, 0x05, 0x01,
];
const PIN_TRIES: u8 = 3;

/// debug 构建里模拟卡的固定种子：密钥跨启动不变，验收用的服务器只需授权一次。
#[cfg(debug_assertions)]
const DEBUG_SEED: [u8; 32] = *b"GuoSSHell virtual OpenPGP card!!";

struct State {
    key: PrivateKey,
    pin: String,
    tries_left: u8,
    verified: bool,
    /// 认证槽开了「按键确认」（UIF）。
    touch: bool,
    /// 模拟按键确认要等的时间。
    touch_delay: Duration,
}

/// 一张模拟卡。克隆出来的是同一张卡（PIN 计数、验证状态共享）。
#[derive(Clone)]
pub struct VirtualCard(Arc<Mutex<State>>);

impl VirtualCard {
    pub fn new(seed: [u8; 32], touch: bool) -> Self {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        Self(Arc::new(Mutex::new(State {
            key,
            pin: PIN.to_owned(),
            tries_left: PIN_TRIES,
            verified: false,
            touch,
            touch_delay: Duration::ZERO,
        })))
    }

    /// debug 构建的模拟卡：固定密钥、开着按键确认（模拟一秒钟的按键）。
    #[cfg(debug_assertions)]
    pub fn for_debug() -> Self {
        let card = Self::new(DEBUG_SEED, true);
        card.state().touch_delay = Duration::from_secs(1);
        card
    }

    #[cfg(test)]
    pub fn public_key(&self) -> PublicKey {
        self.state().key.public_key().clone()
    }

    #[cfg(test)]
    pub fn tries_left(&self) -> u8 {
        self.state().tries_left
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|error| error.into_inner())
    }
}

impl CardReader for VirtualCard {
    fn cards(&self) -> Vec<Box<dyn CardBackend + Send + Sync>> {
        vec![Box::new(self.clone())]
    }
}

impl CardBackend for VirtualCard {
    fn limit_card_caps(&self, card_caps: CardCaps) -> CardCaps {
        card_caps
    }

    fn transaction(
        &mut self,
        _reselect_application: Option<&[u8]>,
    ) -> Result<Box<dyn CardTransaction + Send + Sync + '_>, SmartcardError> {
        Ok(Box::new(self.clone()))
    }
}

impl CardTransaction for VirtualCard {
    fn transmit(&mut self, cmd: &[u8], _buf_size: usize) -> Result<Vec<u8>, SmartcardError> {
        let apdu = Apdu::parse(cmd).ok_or(SmartcardError::Error("malformed APDU".into()))?;
        Ok(self.respond(&apdu))
    }

    fn feature_pinpad_verify(&self) -> bool {
        false
    }

    fn feature_pinpad_modify(&self) -> bool {
        false
    }

    fn pinpad_verify(
        &mut self,
        _pin: PinType,
        _card_caps: &Option<CardCaps>,
    ) -> Result<Vec<u8>, SmartcardError> {
        Err(SmartcardError::Error("no pinpad".into()))
    }

    fn pinpad_modify(
        &mut self,
        _pin: PinType,
        _card_caps: &Option<CardCaps>,
    ) -> Result<Vec<u8>, SmartcardError> {
        Err(SmartcardError::Error("no pinpad".into()))
    }

    fn was_reset(&self) -> bool {
        false
    }
}

const OK: [u8; 2] = [0x90, 0x00];
const NOT_FOUND: [u8; 2] = [0x6A, 0x88];
const SECURITY_NOT_SATISFIED: [u8; 2] = [0x69, 0x82];
const BLOCKED: [u8; 2] = [0x69, 0x83];
const INS_NOT_SUPPORTED: [u8; 2] = [0x6D, 0x00];

impl VirtualCard {
    fn respond(&self, apdu: &Apdu<'_>) -> Vec<u8> {
        let mut state = self.state();
        match (apdu.ins, apdu.p1, apdu.p2) {
            // SELECT：重新选中应用会清掉 PIN 的验证状态。
            (0xA4, 0x04, 0x00) if apdu.data == OPENPGP_AID => {
                state.verified = false;
                OK.to_vec()
            }
            (0xA4, _, _) => vec![0x6A, 0x82],
            (0xCA, 0x00, 0x6E) => with_status(application_related_data(&state)),
            (0xCA, 0x00, 0x65) => with_status(cardholder_related_data()),
            (0xCA, _, _) => NOT_FOUND.to_vec(),
            // 读认证槽（A4）的公钥。
            (0x47, 0x81, 0x00) if apdu.data == [0xA4, 0x00] => {
                with_status(tlv(&[0x7F, 0x49], tlv(&[0x86], ed25519_point(&state.key))))
            }
            (0x47, _, _) => SECURITY_NOT_SATISFIED.to_vec(),
            (0x20, 0x00, 0x81 | 0x82) => verify(&mut state, apdu.data),
            (0x88, 0x00, 0x00) => internal_authenticate(&mut state, apdu.data),
            _ => INS_NOT_SUPPORTED.to_vec(),
        }
    }
}

fn verify(state: &mut State, pin: &[u8]) -> Vec<u8> {
    if state.tries_left == 0 {
        return BLOCKED.to_vec();
    }
    if pin.is_empty() {
        return if state.verified {
            OK.to_vec()
        } else {
            vec![0x63, 0xC0 | state.tries_left]
        };
    }
    if pin == state.pin.as_bytes() {
        state.tries_left = PIN_TRIES;
        state.verified = true;
        return OK.to_vec();
    }
    state.verified = false;
    state.tries_left -= 1;
    if state.tries_left == 0 {
        BLOCKED.to_vec()
    } else {
        vec![0x63, 0xC0 | state.tries_left]
    }
}

fn internal_authenticate(state: &mut State, data: &[u8]) -> Vec<u8> {
    if !state.verified {
        return SECURITY_NOT_SATISFIED.to_vec();
    }
    if state.touch && !state.touch_delay.is_zero() {
        std::thread::sleep(state.touch_delay);
    }
    match state.key.try_sign(data) {
        Ok(signature) => with_status(signature.as_bytes().to_vec()),
        Err(_) => vec![0x6F, 0x00],
    }
}

fn ed25519_point(key: &PrivateKey) -> Vec<u8> {
    key.public_key()
        .key_data()
        .ed25519()
        .map(|point| point.0.to_vec())
        .unwrap_or_default()
}

/// 应用相关数据（6E）：卡号、能力、三个槽的算法、PIN 状态、指纹与按键确认。
fn application_related_data(state: &State) -> Vec<u8> {
    // 签名、解密两槽没有密钥；认证槽的「指纹」取公钥的前 20 字节，只要非零。
    let mut fingerprints = vec![0u8; 40];
    let mut auth = ed25519_point(&state.key);
    auth.resize(20, 0);
    fingerprints.extend(auth);

    let mut discretionary = Vec::new();
    for (tag, value) in [
        (0xC0, vec![0x7D, 0x00, 0x0B, 0xFE, 0x08, 0x00, 0x00, 0xFF, 0x00, 0x00]),
        (0xC1, ED25519.to_vec()),
        (0xC2, X25519.to_vec()),
        (0xC3, ED25519.to_vec()),
        (
            0xC4,
            vec![0xFF, 0x7F, 0x7F, 0x7F, state.tries_left, 0x00, PIN_TRIES],
        ),
        (0xC5, fingerprints),
        (0xC6, vec![0; 60]),
        (0xCD, vec![0; 12]),
        (0xD6, vec![0x00, 0x20]),
        (0xD7, vec![0x00, 0x20]),
        (0xD8, vec![u8::from(state.touch), 0x20]),
    ] {
        discretionary.extend(tlv(&[tag], value));
    }

    let mut data = tlv(&[0x4F], AID.to_vec());
    // 历史字节：卡能力全 0（不用命令链与扩展长度，所有 APDU 都是短格式）。
    data.extend(tlv(
        &[0x5F, 0x52],
        vec![0x00, 0x73, 0x00, 0x00, 0x00, 0x05, 0x90, 0x00],
    ));
    data.extend(tlv(&[0x73], discretionary));
    tlv(&[0x6E], data)
}

fn cardholder_related_data() -> Vec<u8> {
    let mut data = tlv(&[0x5B], CARDHOLDER.as_bytes().to_vec());
    data.extend(tlv(&[0x5F, 0x2D], b"zhen".to_vec()));
    data.extend(tlv(&[0x5F, 0x35], vec![0x39]));
    tlv(&[0x65], data)
}

fn with_status(mut data: Vec<u8>) -> Vec<u8> {
    data.extend_from_slice(&OK);
    data
}

/// BER-TLV：长度按短格式 / 81 / 82 编码。
fn tlv(tag: &[u8], value: Vec<u8>) -> Vec<u8> {
    let mut out = tag.to_vec();
    let length = value.len();
    match u8::try_from(length) {
        Ok(short) if short < 0x80 => out.push(short),
        Ok(short) => out.extend([0x81, short]),
        Err(_) => {
            let long = u16::try_from(length).unwrap_or(u16::MAX);
            out.push(0x82);
            out.extend(long.to_be_bytes());
        }
    }
    out.extend(value);
    out
}

/// 短格式 APDU：CLA INS P1 P2 [Lc 数据] [Le]。
struct Apdu<'a> {
    ins: u8,
    p1: u8,
    p2: u8,
    data: &'a [u8],
}

impl<'a> Apdu<'a> {
    fn parse(cmd: &'a [u8]) -> Option<Self> {
        let (&[_cla, ins, p1, p2], body) = cmd.split_first_chunk::<4>()?;
        let data = match body {
            [] | [_] => &[][..],
            [lc, rest @ ..] => rest.get(..usize::from(*lc))?,
        };
        Some(Self { ins, p1, p2, data })
    }
}
