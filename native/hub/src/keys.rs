//! 私钥：整块存在钥匙串里（generic password；iOS 的 `kSecClassKey` 不参与 iCloud 同步，
//! PLAN §5 M3a），连同名称与公钥打成一个 JSON 信封——同步到别的设备时信封是完整的，
//! 列表靠按 service 搜索钥匙串得到，不另存元数据。口令单独一项，与私钥放在同一个存储
//! （仅本机，或 iCloud 钥匙串）。连接配置的 `identity_file` 写 `keychain:<私钥 id>`。
//!
//! OpenPGP 卡（M3b）与安全密钥（M3d）上的密钥也登记成一个信封：只有公钥与卡号 / 凭据 id，
//! 私钥在卡或安全密钥里。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64ct::{Base64UrlUnpadded, Encoding as _};
use rinf::{DartSignal, RustSignal, debug_print};
use rshell_m0::rshell_core::{AuthenticationKind, ConnectionCatalog, ConnectionProfile};
use rshell_m0::russh::keys::{
    Error as DecodeError, HashAlg, PrivateKey, PublicKey, decode_secret_key,
};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::task::spawn_blocking;
use zeroize::{Zeroize, Zeroizing};

use crate::app::AppContext;
use crate::card::CardInfo;
use crate::security_key::{self, SkFailure};
use crate::signals::keys::{
    AddCardKey, CardScanResult, CardSummary, DeleteKey, ForgetPassphrase, ImportKey, KeyError,
    KeyListState, KeyQuery, KeyResult, KeySummary, RegisterSecurityKey, RenameKey, ScanCards,
    SetKeySync,
};

/// 连接配置里指向钥匙串私钥的 `identity_file` 前缀。
pub const KEY_REF_PREFIX: &str = "keychain:";

/// 钥匙串里的两类条目（按 service 区分，account 是私钥 id）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Item {
    Key,
    Passphrase,
}

impl Item {
    pub fn service(self) -> &'static str {
        match self {
            Self::Key => "guosshell.ssh-key",
            Self::Passphrase => "guosshell.ssh-key-passphrase",
        }
    }
}

#[derive(Debug)]
pub struct StoreError(pub String);

/// 读出的条目内容，以及它是否在 iCloud 钥匙串里。
pub type StoredSecret = (Zeroizing<Vec<u8>>, bool);

/// 私钥与口令的存放处。`synchronized` 选 iCloud 钥匙串或仅本机。
pub trait KeyStore: Send + Sync {
    /// 写入（覆盖同 id 的旧值）。
    fn put(
        &self,
        item: Item,
        id: &str,
        secret: &[u8],
        synchronized: bool,
    ) -> Result<(), StoreError>;
    /// 读出，连同它所在的存储（先本机后 iCloud）。
    fn get(&self, item: Item, id: &str) -> Result<Option<StoredSecret>, StoreError>;
    /// 删除；不存在也算成功。
    fn delete(&self, item: Item, id: &str, synchronized: bool) -> Result<(), StoreError>;
    /// 一个存储里这类条目的全部 id。
    fn list(&self, item: Item, synchronized: bool) -> Result<Vec<String>, StoreError>;
    /// 能否存进 iCloud 钥匙串（同步开关据此可用或不可用）。
    fn sync_available(&self) -> bool;
}

/// Apple 的钥匙串：本机一个存储、iCloud 同步一个存储，都在 protected data 钥匙串里。
/// macOS 上的 protected data 钥匙串要求 App 带团队签名的钥匙串访问组；没有的构建（本地调试的
/// ad-hoc 签名）用登录钥匙串存本机条目，没有 iCloud 同步。
#[cfg(any(target_os = "ios", target_os = "macos"))]
pub struct KeychainKeyStore {
    local: Arc<keyring_core::CredentialStore>,
    cloud: Option<Arc<keyring_core::CredentialStore>>,
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
impl KeychainKeyStore {
    pub fn new() -> Result<Self, String> {
        use apple_native_keyring_store::protected::Store;
        let local = Store::new().map_err(|error| format!("keychain: {error}"))?;
        #[cfg(target_os = "macos")]
        if missing_entitlement(&local) {
            debug_print!("[keys] no keychain access group: using the login keychain");
            let login = apple_native_keyring_store::keychain::Store::new()
                .map_err(|error| format!("login keychain: {error}"))?;
            return Ok(Self {
                local: login,
                cloud: None,
            });
        }
        let cloud_sync = std::collections::HashMap::from([("cloud-sync", "true")]);
        Ok(Self {
            local,
            cloud: Some(
                Store::new_with_configuration(&cloud_sync)
                    .map_err(|error| format!("iCloud keychain: {error}"))?,
            ),
        })
    }

    fn store(&self, synchronized: bool) -> Result<&Arc<keyring_core::CredentialStore>, StoreError> {
        if synchronized {
            self.cloud
                .as_ref()
                .ok_or_else(|| StoreError("iCloud keychain is not available".to_owned()))
        } else {
            Ok(&self.local)
        }
    }

    fn entry(
        &self,
        item: Item,
        id: &str,
        synchronized: bool,
    ) -> Result<keyring_core::Entry, StoreError> {
        self.store(synchronized)?
            .build(item.service(), id, None)
            .map_err(|error| StoreError(error.to_string()))
    }
}

/// protected data 钥匙串能不能用：缺钥匙串访问组时，查询只回「没有」，写入才回
/// `errSecMissingEntitlement`，所以写一个探测条目再删掉。
#[cfg(target_os = "macos")]
fn missing_entitlement(store: &Arc<apple_native_keyring_store::protected::Store>) -> bool {
    use keyring_core::api::CredentialStoreApi;
    const ERR_SEC_MISSING_ENTITLEMENT: i32 = -34018;
    let Ok(probe) = store.build("guosshell.keychain-probe", "probe", None) else {
        return false;
    };
    match probe.set_password("probe") {
        Ok(()) => {
            let _ = probe.delete_credential();
            false
        }
        Err(keyring_core::Error::PlatformFailure(error)) => error
            .downcast_ref::<security_framework::base::Error>()
            .is_some_and(|error| error.code() == ERR_SEC_MISSING_ENTITLEMENT),
        Err(_) => false,
    }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
impl KeyStore for KeychainKeyStore {
    fn put(
        &self,
        item: Item,
        id: &str,
        secret: &[u8],
        synchronized: bool,
    ) -> Result<(), StoreError> {
        self.entry(item, id, synchronized)?
            .set_secret(secret)
            .map_err(|error| StoreError(error.to_string()))
    }

    fn get(&self, item: Item, id: &str) -> Result<Option<StoredSecret>, StoreError> {
        match self.entry(item, id, false)?.get_secret() {
            Ok(secret) => return Ok(Some((Zeroizing::new(secret), false))),
            Err(keyring_core::Error::NoEntry) => {}
            Err(error) => return Err(StoreError(error.to_string())),
        }
        if self.cloud.is_none() {
            return Ok(None);
        }
        // iCloud 钥匙串读不出来时当作没有，不挡住本机条目。
        match self.entry(item, id, true)?.get_secret() {
            Ok(secret) => Ok(Some((Zeroizing::new(secret), true))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => {
                debug_print!("[keys] iCloud keychain read: {error}");
                Ok(None)
            }
        }
    }

    fn delete(&self, item: Item, id: &str, synchronized: bool) -> Result<(), StoreError> {
        match self.entry(item, id, synchronized)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(error) => Err(StoreError(error.to_string())),
        }
    }

    fn list(&self, item: Item, synchronized: bool) -> Result<Vec<String>, StoreError> {
        let Ok(store) = self.store(synchronized) else {
            return Ok(Vec::new());
        };
        let spec = std::collections::HashMap::from([("service", item.service())]);
        match store.search(&spec) {
            Ok(entries) => Ok(entries
                .iter()
                .filter_map(keyring_core::Entry::get_specifiers)
                .map(|(_, account)| account)
                .collect()),
            Err(error) if synchronized => {
                debug_print!("[keys] iCloud keychain search: {error}");
                Ok(Vec::new())
            }
            Err(error) => Err(StoreError(error.to_string())),
        }
    }

    fn sync_available(&self) -> bool {
        self.cloud.is_some()
    }
}

/// 应用偏好（上游设置里没有对应字段的），存在数据目录的 `preferences.json`。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Preferences {
    /// 私钥经 iCloud 钥匙串同步。默认关（PLAN §5 M3a）。
    #[serde(default)]
    pub sync_keys: bool,
    /// 终端下方显示键位条；没设置过时按平台（见 `settings::default_show_key_bar`）。
    #[serde(default)]
    pub show_key_bar: Option<bool>,
}

pub struct PreferenceFile {
    path: PathBuf,
    value: Mutex<Preferences>,
}

impl PreferenceFile {
    pub fn open(path: PathBuf) -> Self {
        let value = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            value: Mutex::new(value),
        }
    }

    pub fn get(&self) -> Preferences {
        *self.value.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// 写临时文件再改名，不会留下半截文件。
    pub fn set(&self, preferences: Preferences) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&preferences).map_err(|error| error.to_string())?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, &self.path).map_err(|error| error.to_string())?;
        *self.value.lock().unwrap_or_else(|error| error.into_inner()) = preferences;
        Ok(())
    }
}

/// 钥匙串里一把私钥的内容。
#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u8,
    name: String,
    /// 私钥原文（可能加密）。
    private_key: String,
    /// OpenSSH 一行格式的公钥。
    public_key: String,
    encrypted: bool,
    /// OpenPGP 卡上的密钥：卡号（`private_key` 为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    card: Option<String>,
    /// 安全密钥上的密钥（`private_key` 为空）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    security_key: Option<SecurityKeyRef>,
}

/// 安全密钥上的凭据：凭据 id（base64url）与 application（WebAuthn 的 RP ID）。
#[derive(Serialize, Deserialize, Clone)]
pub struct SecurityKeyRef {
    pub credential_id: String,
    pub application: String,
}

impl Drop for Envelope {
    fn drop(&mut self) {
        self.private_key.zeroize();
    }
}

impl Envelope {
    fn to_bytes(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        serde_json::to_vec(self)
            .map(Zeroizing::new)
            .map_err(|_| KeyError::Keychain)
    }

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// 连接时用的私钥（原文与已存的口令），或 OpenPGP 卡的卡号与公钥。
pub struct StoredKey {
    pub name: String,
    pub private_key: SecretString,
    pub encrypted: bool,
    pub synchronized: bool,
    pub passphrase: Option<SecretString>,
    /// OpenPGP 卡上的密钥：卡号。
    pub card: Option<String>,
    /// 安全密钥上的密钥。
    pub security_key: Option<SecurityKeyRef>,
    /// OpenSSH 一行格式的公钥。
    pub public_key: String,
}

/// 连接配置引用的私钥 id（`identity_file` 是 `keychain:<id>` 时）。
pub fn key_id(profile: &ConnectionProfile) -> Option<&str> {
    profile
        .identity_file
        .as_ref()?
        .to_str()?
        .strip_prefix(KEY_REF_PREFIX)
        .filter(|id| !id.is_empty())
}

pub fn key_ref(id: &str) -> PathBuf {
    PathBuf::from(format!("{KEY_REF_PREFIX}{id}"))
}

/// 解出私钥。加密私钥要口令；口令错与缺都按 `KeyError` 区分。
pub fn decode(private_key: &str, passphrase: Option<&str>) -> Result<PrivateKey, KeyError> {
    decode_secret_key(private_key, passphrase).map_err(|error| match (error, passphrase) {
        (DecodeError::KeyIsEncrypted, _) => KeyError::PassphraseRequired,
        (_, Some(_)) => KeyError::PassphraseWrong,
        (_, None) => KeyError::Invalid,
    })
}

/// 读出私钥的公钥与是否加密。OpenSSH 格式的加密私钥不用口令也能读出公钥；
/// 其他格式要口令解开才有。给了口令就顺带验证它。
fn inspect(private_key: &str, passphrase: Option<&str>) -> Result<(PublicKey, bool), KeyError> {
    match decode(private_key, None) {
        Ok(key) => Ok((key.public_key().clone(), false)),
        Err(KeyError::PassphraseRequired) => match passphrase {
            Some(passphrase) => {
                decode(private_key, Some(passphrase)).map(|key| (key.public_key().clone(), true))
            }
            None => PrivateKey::from_openssh(private_key)
                .map(|key| (key.public_key().clone(), true))
                .map_err(|_| KeyError::PassphraseRequired),
        },
        Err(error) => Err(error),
    }
}

fn store_error(synchronized: bool) -> impl Fn(StoreError) -> KeyError {
    move |error| {
        debug_print!("[keys] keychain: {}", error.0);
        if synchronized {
            KeyError::SyncUnavailable
        } else {
            KeyError::Keychain
        }
    }
}

fn load_envelope(store: &dyn KeyStore, id: &str) -> Result<Option<(Envelope, bool)>, KeyError> {
    let Some((bytes, synchronized)) = store.get(Item::Key, id).map_err(store_error(false))? else {
        return Ok(None);
    };
    Ok(Envelope::from_bytes(&bytes).map(|envelope| (envelope, synchronized)))
}

/// 全部私钥 id（本机与 iCloud 钥匙串），按存储标注。
fn all_ids(store: &dyn KeyStore) -> Result<BTreeMap<String, bool>, KeyError> {
    let mut ids = BTreeMap::new();
    for synchronized in [true, false] {
        for id in store
            .list(Item::Key, synchronized)
            .map_err(store_error(false))?
        {
            ids.insert(id, synchronized);
        }
    }
    Ok(ids)
}

/// 导入私钥，返回它的 id。名称为空时用私钥的注释，再没有就用算法名。
pub fn import(
    context: &AppContext,
    name: &str,
    private_key: &str,
    passphrase: &str,
) -> Result<String, KeyError> {
    let private_key = private_key.trim();
    let passphrase = Some(passphrase).filter(|passphrase| !passphrase.is_empty());
    let (public_key, encrypted) = inspect(private_key, passphrase)?;
    reject_duplicate(context, &public_key)?;

    let comment = public_key.comment().as_str_lossy().trim().to_owned();
    let name = match name.trim() {
        "" if !comment.is_empty() => comment,
        "" => public_key.algorithm().as_str().to_owned(),
        name => name.to_owned(),
    };
    store_new(
        context,
        &Envelope {
            version: 1,
            name,
            private_key: private_key.to_owned(),
            public_key: public_key.to_openssh().map_err(|_| KeyError::Invalid)?,
            encrypted,
            card: None,
            security_key: None,
        },
    )
}

/// 登记 OpenPGP 卡的认证密钥（上次读卡看到的卡号为 `ident` 的卡），返回 id。
/// 名称为空时用持卡人名，再没有就用卡号。
pub fn add_card(context: &AppContext, ident: &str, name: &str) -> Result<String, KeyError> {
    let card = context.cards.scanned(ident).ok_or(KeyError::CardNotFound)?;
    let public_key = card.public_key.ok_or(KeyError::CardUnsupported)?;
    reject_duplicate(context, &public_key)?;
    let name = match name.trim() {
        "" if !card.cardholder.is_empty() => card.cardholder.clone(),
        "" => format!("OpenPGP 卡 {ident}"),
        name => name.to_owned(),
    };
    store_new(
        context,
        &Envelope {
            version: 1,
            name,
            private_key: String::new(),
            public_key: public_key.to_openssh().map_err(|_| KeyError::Invalid)?,
            encrypted: false,
            card: Some(ident.to_owned()),
            security_key: None,
        },
    )
}

/// 在安全密钥上新建一把凭据并登记，返回 id。系统界面引导用户插上、靠近或触摸安全密钥。
pub fn add_security_key(context: &AppContext, name: &str) -> Result<String, KeyError> {
    let name = match name.trim() {
        "" => "安全密钥".to_owned(),
        name => name.to_owned(),
    };
    let rp_id = context
        .security_keys
        .relying_party()
        .ok_or(KeyError::SecurityKeyUnavailable)?;
    let registration = context
        .security_keys
        .register(&rp_id, &name)
        .map_err(security_key_error)?;
    let mut public_key =
        security_key::public_key(&registration, &rp_id).map_err(security_key_error)?;
    // 公钥注释用名字，贴进 authorized_keys 后认得出是哪把。
    public_key.set_comment(name.as_str());
    reject_duplicate(context, &public_key)?;
    store_new(
        context,
        &Envelope {
            version: 1,
            name,
            private_key: String::new(),
            public_key: public_key.to_openssh().map_err(|_| KeyError::Invalid)?,
            encrypted: false,
            card: None,
            security_key: Some(SecurityKeyRef {
                credential_id: Base64UrlUnpadded::encode_string(&registration.credential_id),
                application: rp_id,
            }),
        },
    )
}

fn security_key_error(failure: SkFailure) -> KeyError {
    debug_print!("[keys] security key: {failure:?}");
    match failure {
        SkFailure::Unavailable => KeyError::SecurityKeyUnavailable,
        SkFailure::Cancelled => KeyError::SecurityKeyCancelled,
        SkFailure::Invalid(_) | SkFailure::Failed(_) => KeyError::SecurityKeyFailed,
    }
}

/// 读卡，并标出已经登记过的。
pub fn scan_cards(context: &AppContext, nfc: bool) -> Result<Vec<CardSummary>, KeyError> {
    let cards = context.cards.scan(nfc).map_err(|failure| {
        debug_print!("[keys] card scan: {failure:?}");
        KeyError::CardNotFound
    })?;
    let known = known_fingerprints(context)?;
    Ok(cards
        .into_iter()
        .map(|card| card_summary(card, &known))
        .collect())
}

fn card_summary(card: CardInfo, known: &[String]) -> CardSummary {
    let (public_key, fingerprint) = card
        .public_key
        .as_ref()
        .map(|key| {
            (
                key.to_openssh().unwrap_or_default(),
                key.fingerprint(HashAlg::Sha256).to_string(),
            )
        })
        .unwrap_or_default();
    CardSummary {
        added: !fingerprint.is_empty() && known.contains(&fingerprint),
        ident: card.ident,
        cardholder: card.cardholder,
        algorithm: card.algorithm,
        public_key,
        fingerprint,
        pin_tries_left: u32::from(card.pin_tries_left),
        touch: card.touch,
    }
}

/// 已登记的全部公钥指纹。
fn known_fingerprints(context: &AppContext) -> Result<Vec<String>, KeyError> {
    let mut known = Vec::new();
    for id in all_ids(context.keys.as_ref())?.keys() {
        if let Some((envelope, _)) = load_envelope(context.keys.as_ref(), id)?
            && let Some(fingerprint) = fingerprint_of(&envelope.public_key)
        {
            known.push(fingerprint);
        }
    }
    Ok(known)
}

/// 同一把公钥只登记一次。
fn reject_duplicate(context: &AppContext, public_key: &PublicKey) -> Result<(), KeyError> {
    let fingerprint = public_key.fingerprint(HashAlg::Sha256).to_string();
    if known_fingerprints(context)?.contains(&fingerprint) {
        return Err(KeyError::AlreadyExists);
    }
    Ok(())
}

/// 存一个新信封（按同步开关选存储），返回新 id。
fn store_new(context: &AppContext, envelope: &Envelope) -> Result<String, KeyError> {
    let id = uuid::Uuid::new_v4().to_string();
    let synchronized = context.preferences.get().sync_keys;
    context
        .keys
        .put(Item::Key, &id, &envelope.to_bytes()?, synchronized)
        .map_err(store_error(synchronized))?;
    Ok(id)
}

pub fn rename(context: &AppContext, id: &str, name: &str) -> Result<(), KeyError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(KeyError::Invalid);
    }
    let (mut envelope, synchronized) =
        load_envelope(context.keys.as_ref(), id)?.ok_or(KeyError::NotFound)?;
    envelope.name = name.to_owned();
    context
        .keys
        .put(Item::Key, id, &envelope.to_bytes()?, synchronized)
        .map_err(store_error(synchronized))
}

/// 删除私钥与它存下的口令。有连接在用时拒绝。
pub fn delete(context: &AppContext, catalog: &ConnectionCatalog, id: &str) -> Result<(), KeyError> {
    if load_envelope(context.keys.as_ref(), id)?.is_none() {
        return Err(KeyError::NotFound);
    }
    if used_by(catalog, id) > 0 {
        return Err(KeyError::InUse);
    }
    remove(context.keys.as_ref(), Item::Passphrase, id)?;
    remove(context.keys.as_ref(), Item::Key, id)
}

/// 删掉存下的口令，下次连接时再问。
pub fn forget_passphrase(context: &AppContext, id: &str) -> Result<(), KeyError> {
    if load_envelope(context.keys.as_ref(), id)?.is_none() {
        return Err(KeyError::NotFound);
    }
    remove(context.keys.as_ref(), Item::Passphrase, id)
}

/// 从条目所在的存储删掉它。切换同步中途被打断时同一项可能两处都有，删到读不到为止；
/// 不去碰没有这一项的存储（iCloud 钥匙串不可用时照样删得掉本机条目）。
fn remove(store: &dyn KeyStore, item: Item, id: &str) -> Result<(), KeyError> {
    for _ in 0..2 {
        let Some((_, synchronized)) = store.get(item, id).map_err(store_error(false))? else {
            return Ok(());
        };
        store
            .delete(item, id, synchronized)
            .map_err(store_error(synchronized))?;
    }
    Ok(())
}

/// 开关 iCloud 同步：私钥与口令整体搬到另一个存储——先全部写进目标，
/// 有一项写不进就撤回已写的，全部成功后才删原处的。
pub fn set_sync(context: &AppContext, enabled: bool) -> Result<(), KeyError> {
    let mut preferences = context.preferences.get();
    if preferences.sync_keys == enabled {
        return Ok(());
    }
    if enabled && !context.keys.sync_available() {
        return Err(KeyError::SyncUnavailable);
    }
    let store = context.keys.as_ref();
    let mut moving = Vec::new();
    for item in [Item::Key, Item::Passphrase] {
        for id in store.list(item, !enabled).map_err(store_error(false))? {
            moving.push((item, id));
        }
    }
    let mut written: Vec<(Item, String)> = Vec::new();
    for (item, id) in &moving {
        let copied = store
            .get(*item, id)
            .map_err(store_error(false))
            .and_then(|found| found.ok_or(KeyError::NotFound))
            .and_then(|(secret, _)| {
                store
                    .put(*item, id, &secret, enabled)
                    .map_err(store_error(enabled))
            });
        if let Err(error) = copied {
            for (item, id) in &written {
                let _ = store.delete(*item, id, enabled);
            }
            return Err(error);
        }
        written.push((*item, id.clone()));
    }
    for (item, id) in &moving {
        store
            .delete(*item, id, !enabled)
            .map_err(store_error(false))?;
    }
    preferences.sync_keys = enabled;
    context.preferences.set(preferences).map_err(|error| {
        debug_print!("[keys] preferences: {error}");
        KeyError::Keychain
    })
}

/// 连接用：读出私钥与存下的口令。
pub fn load(context: &AppContext, id: &str) -> Result<Option<StoredKey>, KeyError> {
    let Some((envelope, synchronized)) = load_envelope(context.keys.as_ref(), id)? else {
        return Ok(None);
    };
    let passphrase = context
        .keys
        .get(Item::Passphrase, id)
        .map_err(store_error(false))?
        .and_then(|(bytes, _)| String::from_utf8(bytes.to_vec()).ok())
        .map(SecretString::from);
    Ok(Some(StoredKey {
        name: envelope.name.clone(),
        private_key: SecretString::from(envelope.private_key.clone()),
        encrypted: envelope.encrypted,
        synchronized,
        passphrase,
        card: envelope.card.clone(),
        security_key: envelope.security_key.clone(),
        public_key: envelope.public_key.clone(),
    }))
}

/// 存下（已验证过的）口令，与私钥放在同一个存储。
pub fn save_passphrase(
    context: &AppContext,
    id: &str,
    passphrase: &SecretString,
    synchronized: bool,
) -> Result<(), KeyError> {
    context
        .keys
        .put(
            Item::Passphrase,
            id,
            passphrase.expose_secret().as_bytes(),
            synchronized,
        )
        .map_err(store_error(synchronized))
}

fn used_by(catalog: &ConnectionCatalog, id: &str) -> u32 {
    let count = catalog
        .connections
        .values()
        .filter(|profile| {
            profile.authentication == AuthenticationKind::PublicKey && key_id(profile) == Some(id)
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

fn fingerprint_of(public_key: &str) -> Option<String> {
    PublicKey::from_openssh(public_key)
        .ok()
        .map(|key| key.fingerprint(HashAlg::Sha256).to_string())
}

/// 列表（按名称排序）。读不出的条目跳过。
pub fn summaries(
    context: &AppContext,
    catalog: &ConnectionCatalog,
) -> Result<Vec<KeySummary>, KeyError> {
    let mut keys = Vec::new();
    for id in all_ids(context.keys.as_ref())?.keys() {
        let Some((envelope, synchronized)) = load_envelope(context.keys.as_ref(), id)? else {
            continue;
        };
        let Ok(public_key) = PublicKey::from_openssh(&envelope.public_key) else {
            continue;
        };
        let passphrase_saved = context
            .keys
            .get(Item::Passphrase, id)
            .map_err(store_error(false))?
            .is_some();
        keys.push(KeySummary {
            id: id.clone(),
            name: envelope.name.clone(),
            algorithm: public_key.algorithm().as_str().to_owned(),
            fingerprint: public_key.fingerprint(HashAlg::Sha256).to_string(),
            public_key: envelope.public_key.clone(),
            encrypted: envelope.encrypted,
            passphrase_saved,
            synchronized,
            used_by: used_by(catalog, id),
            card_ident: envelope.card.clone().unwrap_or_default(),
            security_key: envelope.security_key.is_some(),
        });
    }
    keys.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    Ok(keys)
}

pub async fn run(context: Arc<AppContext>) {
    let query_rx = KeyQuery::get_dart_signal_receiver();
    let import_rx = ImportKey::get_dart_signal_receiver();
    let rename_rx = RenameKey::get_dart_signal_receiver();
    let delete_rx = DeleteKey::get_dart_signal_receiver();
    let forget_rx = ForgetPassphrase::get_dart_signal_receiver();
    let sync_rx = SetKeySync::get_dart_signal_receiver();
    let scan_rx = ScanCards::get_dart_signal_receiver();
    let add_card_rx = AddCardKey::get_dart_signal_receiver();
    let security_key_rx = RegisterSecurityKey::get_dart_signal_receiver();
    loop {
        tokio::select! {
            pack = query_rx.recv() => {
                if pack.is_none() {
                    break;
                }
            }
            pack = import_rx.recv() => {
                let Some(pack) = pack else { break };
                let request = pack.message;
                let request_id = request.request_id;
                let result = blocking(&context, move |context| {
                    let private_key = Zeroizing::new(request.private_key);
                    let passphrase = Zeroizing::new(request.passphrase);
                    import(context, &request.name, &private_key, &passphrase)
                })
                .await;
                reply(request_id, result);
            }
            pack = rename_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let result = blocking(&context, move |context| {
                    rename(context, &pack.message.id, &pack.message.name)
                })
                .await;
                reply(request_id, result.map(|()| String::new()));
            }
            pack = delete_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let result = blocking(&context, move |context| {
                    let catalog = context.repository.load_catalog().map_err(|_| KeyError::Keychain)?;
                    delete(context, &catalog, &pack.message.id)
                })
                .await;
                reply(request_id, result.map(|()| String::new()));
            }
            pack = forget_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let result = blocking(&context, move |context| {
                    forget_passphrase(context, &pack.message.id)
                })
                .await;
                reply(request_id, result.map(|()| String::new()));
            }
            pack = scan_rx.recv() => {
                let Some(pack) = pack else { break };
                let request = pack.message;
                let nfc_available = context.cards.reader.nfc_supported();
                let (error, cards) = match blocking(&context, move |context| {
                    scan_cards(context, request.nfc)
                })
                .await
                {
                    Ok(cards) => (KeyError::None, cards),
                    Err(error) => (error, Vec::new()),
                };
                CardScanResult {
                    request_id: request.request_id,
                    error,
                    cards,
                    nfc_available,
                }
                .send_signal_to_dart();
                continue;
            }
            pack = add_card_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let result = blocking(&context, move |context| {
                    add_card(context, &pack.message.ident, &pack.message.name)
                })
                .await;
                reply(request_id, result);
            }
            pack = security_key_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let result = blocking(&context, move |context| {
                    add_security_key(context, &pack.message.name)
                })
                .await;
                reply(request_id, result);
            }
            pack = sync_rx.recv() => {
                let Some(pack) = pack else { break };
                let request_id = pack.message.request_id;
                let enabled = pack.message.enabled;
                let result = blocking(&context, move |context| set_sync(context, enabled)).await;
                reply(request_id, result.map(|()| String::new()));
            }
            () = context.keys_changed.notified() => {}
        }
        publish(&context).await;
    }
}

async fn blocking<T: Send + 'static>(
    context: &Arc<AppContext>,
    operation: impl FnOnce(&AppContext) -> Result<T, KeyError> + Send + 'static,
) -> Result<T, KeyError> {
    let context = context.clone();
    spawn_blocking(move || operation(&context))
        .await
        .unwrap_or(Err(KeyError::Keychain))
}

fn reply(request_id: u32, result: Result<String, KeyError>) {
    let (error, key_id) = match result {
        Ok(key_id) => (KeyError::None, key_id),
        Err(error) => (error, String::new()),
    };
    KeyResult {
        request_id,
        error,
        key_id,
    }
    .send_signal_to_dart();
}

async fn publish(context: &Arc<AppContext>) {
    let listed = blocking(context, |context| {
        let catalog = context
            .repository
            .load_catalog()
            .map_err(|_| KeyError::Keychain)?;
        summaries(context, &catalog)
    })
    .await;
    match listed {
        Ok(keys) => KeyListState {
            keys,
            sync_enabled: context.preferences.get().sync_keys,
            sync_available: context.keys.sync_available(),
            security_keys_available: context.security_keys.relying_party().is_some(),
        }
        .send_signal_to_dart(),
        Err(error) => debug_print!("[keys] list: {error:?}"),
    }
}

/// 测试用的内存存储；`cloud_available = false` 模拟 iCloud 钥匙串不可用。
#[cfg(test)]
pub struct MemoryKeyStore {
    items: Mutex<BTreeMap<(Item, String, bool), Vec<u8>>>,
    pub cloud_available: bool,
}

#[cfg(test)]
impl MemoryKeyStore {
    pub fn new(cloud_available: bool) -> Self {
        Self {
            items: Mutex::new(BTreeMap::new()),
            cloud_available,
        }
    }

    fn items(&self) -> std::sync::MutexGuard<'_, BTreeMap<(Item, String, bool), Vec<u8>>> {
        self.items.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[cfg(test)]
impl KeyStore for MemoryKeyStore {
    fn put(
        &self,
        item: Item,
        id: &str,
        secret: &[u8],
        synchronized: bool,
    ) -> Result<(), StoreError> {
        if synchronized && !self.cloud_available {
            return Err(StoreError("missing iCloud entitlement".to_owned()));
        }
        self.items()
            .insert((item, id.to_owned(), synchronized), secret.to_vec());
        Ok(())
    }

    fn get(&self, item: Item, id: &str) -> Result<Option<StoredSecret>, StoreError> {
        let items = self.items();
        Ok([false, true].into_iter().find_map(|synchronized| {
            items
                .get(&(item, id.to_owned(), synchronized))
                .map(|secret| (Zeroizing::new(secret.clone()), synchronized))
        }))
    }

    fn delete(&self, item: Item, id: &str, synchronized: bool) -> Result<(), StoreError> {
        if synchronized && !self.cloud_available {
            return Err(StoreError("missing iCloud entitlement".to_owned()));
        }
        self.items().remove(&(item, id.to_owned(), synchronized));
        Ok(())
    }

    fn list(&self, item: Item, synchronized: bool) -> Result<Vec<String>, StoreError> {
        Ok(self
            .items()
            .keys()
            .filter(|(kind, _, cloud)| *kind == item && *cloud == synchronized)
            .map(|(_, id, _)| id.clone())
            .collect())
    }

    fn sync_available(&self) -> bool {
        self.cloud_available
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::{
        Item, MemoryKeyStore, PreferenceFile, add_card, add_security_key, decode, delete,
        forget_passphrase, import, key_ref, load, rename, save_passphrase, scan_cards, set_sync,
        summaries,
    };
    use crate::app::AppContext;
    use crate::card::virtual_card::{CARDHOLDER, CardKey, IDENT, VirtualCard};
    use crate::card::{CardContext, NoCards};
    use crate::signals::keys::KeyError;
    use rshell_m0::rshell_core::{
        AuthenticationKind, CatalogMutation, ConnectionCatalog, ConnectionProfile, TransportKind,
    };
    use rshell_m0::rshell_storage::{
        CredentialCoordinator, MemoryCredentialVault, SqliteRepository,
    };
    use rshell_m0::russh::keys::ssh_key::LineEnding;
    use rshell_m0::russh::keys::{Algorithm, PrivateKey, PublicKey, key::safe_rng};
    use secrecy::{ExposeSecret, SecretString};
    use tokio::sync::Notify;

    /// 一次性测试样本：传统 PEM 格式、AES-128-CBC 加密的 RSA 私钥。
    const RSA_PEM_ENCRYPTED: &str = include_str!("testdata/rsa-pem-encrypted.key");
    const RSA_PEM_PASSPHRASE: &str = "test-passphrase";

    fn context(cloud_available: bool) -> AppContext {
        let repository = Arc::new(SqliteRepository::open_in_memory().expect("in-memory catalog"));
        repository.migrate().expect("migrate");
        let preferences =
            std::env::temp_dir().join(format!("guosh-keys-test-{}.json", uuid::Uuid::new_v4()));
        AppContext {
            credentials: CredentialCoordinator::new(
                repository.clone(),
                Arc::new(MemoryCredentialVault::new()),
            ),
            repository,
            known_hosts: PathBuf::new(),
            keys: Arc::new(MemoryKeyStore::new(cloud_available)),
            cards: Arc::new(CardContext::new(Arc::new(NoCards))),
            security_keys: Arc::new(crate::security_key::Unavailable),
            preferences: PreferenceFile::open(preferences),
            catalog_changed: Notify::new(),
            keys_changed: Notify::new(),
        }
    }

    fn ed25519(passphrase: Option<&str>) -> String {
        commented_ed25519(passphrase, "")
    }

    fn commented_ed25519(passphrase: Option<&str>, comment: &str) -> String {
        let mut key = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).expect("key");
        key.set_comment(comment);
        let key = match passphrase {
            Some(passphrase) => key.encrypt(&mut safe_rng(), passphrase).expect("encrypt"),
            None => key,
        };
        key.to_openssh(LineEnding::LF).expect("openssh").to_string()
    }

    fn catalog() -> ConnectionCatalog {
        ConnectionCatalog::default()
    }

    #[test]
    fn keys_import_with_or_without_a_passphrase_where_the_format_allows() {
        let context = context(false);
        let plain = import(&context, "laptop", &ed25519(None), "").expect("plain key");
        // OpenSSH 格式的加密私钥：公钥不加密，不用口令也能导入。
        let encrypted = import(&context, "", &ed25519(Some("secret")), "").expect("encrypted key");
        // 传统 PEM 加密私钥要口令才读得出公钥。
        assert_eq!(
            import(&context, "rsa", RSA_PEM_ENCRYPTED, ""),
            Err(KeyError::PassphraseRequired)
        );
        assert_eq!(
            import(&context, "rsa", RSA_PEM_ENCRYPTED, "wrong"),
            Err(KeyError::PassphraseWrong)
        );
        let rsa = import(&context, "", RSA_PEM_ENCRYPTED, RSA_PEM_PASSPHRASE).expect("rsa");
        let commented =
            import(&context, " ", &commented_ed25519(None, "me@laptop"), "").expect("commented");
        assert_eq!(
            import(&context, "again", RSA_PEM_ENCRYPTED, RSA_PEM_PASSPHRASE),
            Err(KeyError::AlreadyExists)
        );
        assert_eq!(
            import(&context, "junk", "not a key", ""),
            Err(KeyError::Invalid)
        );

        let keys = summaries(&context, &catalog()).expect("list");
        let find = |id: &str| keys.iter().find(|key| key.id == id).expect("listed");
        assert_eq!(find(&plain).name, "laptop");
        assert!(!find(&plain).encrypted);
        assert_eq!(find(&plain).algorithm, "ssh-ed25519");
        assert!(find(&encrypted).encrypted);
        assert_eq!(
            find(&encrypted).name,
            "ssh-ed25519",
            "no name, no comment: the algorithm"
        );
        assert_eq!(
            find(&commented).name,
            "me@laptop",
            "no name: the key comment"
        );
        assert_eq!(find(&rsa).name, "ssh-rsa", "PEM keys carry no comment");
        assert_eq!(find(&rsa).algorithm, "ssh-rsa");
        assert!(find(&rsa).fingerprint.starts_with("SHA256:"));
        assert!(find(&rsa).public_key.starts_with("ssh-rsa "));
    }

    #[test]
    fn stored_keys_decode_with_their_passphrase() {
        let context = context(false);
        let id = import(&context, "", RSA_PEM_ENCRYPTED, RSA_PEM_PASSPHRASE).expect("rsa");
        let stored = load(&context, &id).expect("load").expect("stored");
        assert!(stored.encrypted);
        assert!(stored.passphrase.is_none());
        assert_eq!(
            decode(stored.private_key.expose_secret(), None).err(),
            Some(KeyError::PassphraseRequired)
        );
        assert_eq!(
            decode(stored.private_key.expose_secret(), Some("wrong")).err(),
            Some(KeyError::PassphraseWrong)
        );
        decode(stored.private_key.expose_secret(), Some(RSA_PEM_PASSPHRASE)).expect("decrypts");

        save_passphrase(
            &context,
            &id,
            &SecretString::from(RSA_PEM_PASSPHRASE.to_owned()),
            false,
        )
        .expect("save passphrase");
        let stored = load(&context, &id).expect("load").expect("stored");
        assert_eq!(
            stored
                .passphrase
                .as_ref()
                .map(|p| p.expose_secret().to_owned())
                .as_deref(),
            Some(RSA_PEM_PASSPHRASE)
        );
        assert!(summaries(&context, &catalog()).expect("list")[0].passphrase_saved);

        forget_passphrase(&context, &id).expect("forget");
        assert!(
            load(&context, &id)
                .expect("load")
                .expect("stored")
                .passphrase
                .is_none()
        );
        assert!(!summaries(&context, &catalog()).expect("list")[0].passphrase_saved);
        assert_eq!(
            forget_passphrase(&context, "missing"),
            Err(KeyError::NotFound)
        );
    }

    #[test]
    fn keys_in_use_cannot_be_deleted_and_deleting_removes_the_passphrase_too() {
        // iCloud 钥匙串不可用：删除只碰本机存储。
        let context = context(false);
        let id = import(&context, "work", &ed25519(None), "").expect("key");
        rename(&context, &id, "  renamed  ").expect("rename");
        assert_eq!(
            summaries(&context, &catalog()).expect("list")[0].name,
            "renamed"
        );
        save_passphrase(&context, &id, &SecretString::from("x".to_owned()), false).expect("save");

        let mut profile = ConnectionProfile::new("server", "server.test");
        profile.transport = TransportKind::NativeSsh;
        profile.authentication = AuthenticationKind::PublicKey;
        profile.identity_file = Some(key_ref(&id));
        let mut in_use = catalog();
        in_use
            .apply(CatalogMutation::Create(profile))
            .expect("connection");
        assert_eq!(summaries(&context, &in_use).expect("list")[0].used_by, 1);
        assert_eq!(delete(&context, &in_use, &id), Err(KeyError::InUse));

        delete(&context, &catalog(), &id).expect("delete");
        assert!(context.keys.get(Item::Key, &id).expect("get").is_none());
        assert!(
            context
                .keys
                .get(Item::Passphrase, &id)
                .expect("get")
                .is_none()
        );
        assert_eq!(delete(&context, &catalog(), &id), Err(KeyError::NotFound));
    }

    #[test]
    fn sync_moves_keys_and_passphrases_and_rolls_back_without_icloud() {
        let unavailable = context(false);
        let id = import(&unavailable, "k", &ed25519(None), "").expect("key");
        assert_eq!(set_sync(&unavailable, true), Err(KeyError::SyncUnavailable));
        assert!(!unavailable.preferences.get().sync_keys);
        assert_eq!(
            unavailable.keys.list(Item::Key, false).expect("list"),
            std::slice::from_ref(&id)
        );

        let available = context(true);
        let id = import(&available, "k", &ed25519(None), "").expect("key");
        save_passphrase(&available, &id, &SecretString::from("x".to_owned()), false).expect("save");
        set_sync(&available, true).expect("enable");
        assert!(available.preferences.get().sync_keys);
        assert!(
            available
                .keys
                .list(Item::Key, false)
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            available.keys.list(Item::Passphrase, true).expect("list"),
            std::slice::from_ref(&id)
        );
        assert!(summaries(&available, &catalog()).expect("list")[0].synchronized);
        // 同步开着时新导入的也进 iCloud 钥匙串。
        let second = import(&available, "k2", &ed25519(None), "").expect("key");
        assert!(
            available
                .keys
                .list(Item::Key, true)
                .expect("list")
                .contains(&second)
        );

        set_sync(&available, false).expect("disable");
        assert!(
            available
                .keys
                .list(Item::Key, true)
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            available.keys.list(Item::Key, false).expect("list").len(),
            2
        );
    }

    #[test]
    fn a_scanned_card_is_registered_once_and_loads_as_a_card_key() {
        let mut context = context(false);
        let card = VirtualCard::new(CardKey::ed25519([7; 32]), false);
        context.cards = Arc::new(CardContext::new(Arc::new(card.clone())));

        assert_eq!(add_card(&context, IDENT, ""), Err(KeyError::CardNotFound));
        let scanned = scan_cards(&context, false).expect("scan");
        assert_eq!(scanned.len(), 1);
        assert!(!scanned[0].added);
        assert!(scanned[0].public_key.starts_with("ssh-ed25519 "));

        let id = add_card(&context, IDENT, "").expect("register");
        assert_eq!(
            add_card(&context, IDENT, "again"),
            Err(KeyError::AlreadyExists)
        );
        assert!(scan_cards(&context, false).expect("scan")[0].added);

        let listed = summaries(&context, &catalog()).expect("list");
        assert_eq!(listed[0].name, CARDHOLDER);
        assert_eq!(listed[0].card_ident, IDENT);
        assert!(!listed[0].encrypted);

        let stored = load(&context, &id).expect("load").expect("stored");
        assert_eq!(stored.card.as_deref(), Some(IDENT));
        assert!(stored.private_key.expose_secret().is_empty());
        let public_key = PublicKey::from_openssh(&stored.public_key).expect("public key");
        assert_eq!(public_key.key_data(), card.public_key().key_data());
        assert_eq!(public_key.comment().as_str_lossy(), "cardno:FFFF00000001");
    }

    #[test]
    fn a_security_key_credential_is_registered_once_and_loads_as_a_reference() {
        let mut context = context(false);
        assert_eq!(
            add_security_key(&context, "yubikey"),
            Err(KeyError::SecurityKeyUnavailable)
        );

        context.security_keys = Arc::new(
            crate::security_key::virtual_key::VirtualSecurityKey::new([9; 32]).expect("key"),
        );
        let id = add_security_key(&context, " yubikey ").expect("register");
        assert_eq!(
            add_security_key(&context, "again"),
            Err(KeyError::AlreadyExists),
            "the virtual key always returns the same credential"
        );

        let listed = summaries(&context, &catalog()).expect("list");
        assert_eq!(listed[0].name, "yubikey");
        assert!(listed[0].security_key);
        assert!(listed[0].card_ident.is_empty());

        let stored = load(&context, &id).expect("load").expect("stored");
        assert!(stored.private_key.expose_secret().is_empty());
        let reference = stored.security_key.expect("security key reference");
        assert_eq!(reference.application, "ssh:");
        assert!(!reference.credential_id.is_empty());
        let public_key = PublicKey::from_openssh(&stored.public_key).expect("public key");
        assert_eq!(public_key.algorithm(), Algorithm::SkEcdsaSha2NistP256);
        assert_eq!(public_key.comment().as_str_lossy(), "yubikey");
    }
}
