//! 建立连接：连接配置（目录里的，或快速连接的临时目标）→ 认证材料 → 握手
//! （含主机密钥确认）。
//!
//! 期间要用户回答的问题——密码、主机密钥确认、keyboard-interactive——发成
//! `InteractionPrompt`，回答经会话的 `replies` 通道回来。用户在此期间的其他操作
//! （输入、改尺寸）存进 backlog，连上后按原顺序处理；Disconnect 立即中止连接。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use rinf::{RustSignal, debug_print};
use rshell_m0::rshell_core::{
    AuthenticationKind, CatalogMutation, ConnectionId, ConnectionProfile, HostKeyDecision,
    HostKeyPrompt, InteractionId, InteractionRequest, InteractionResponse,
    KeyboardInteractivePrompt, SecretUpdate, SessionFailure, TerminalSize, TransportKind,
};
use rshell_m0::rshell_session::{
    AuthPlan, InteractionBroker, KnownHostsVerifier, NativeSshTransport, SessionTransport,
    TransportRequest, interaction_channel,
};
use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::spawn_blocking;

use crate::app::AppContext;
use crate::catalog;
use crate::session::SessionCommand;
use crate::signals::interaction::{InteractionPrompt, InteractionReply, PromptField, PromptKind};
use crate::signals::{ConnectRequest, FailureKind};

pub struct Connected {
    pub transport: NativeSshTransport,
    pub profile: ConnectionProfile,
}

/// 连接没有建成的原因。
pub enum Abort {
    /// 用户取消：关掉了页面，或取消了密码框。
    Cancelled,
    Failed {
        failure: FailureKind,
        detail: String,
    },
}

impl Abort {
    fn failed(failure: FailureKind, detail: String) -> Self {
        Self::Failed { failure, detail }
    }
}

/// 连接过程中会话收到的东西。
pub struct Channels<'a> {
    pub commands: &'a mut UnboundedReceiver<SessionCommand>,
    pub replies: &'a mut UnboundedReceiver<InteractionReply>,
    /// 连接期间收到的非 Disconnect 命令，连上后按顺序处理。
    pub backlog: &'a mut VecDeque<SessionCommand>,
}

impl Channels<'_> {
    /// 连接期间收到命令：Disconnect（或会话被丢弃）即中止，其余留到连上以后。
    fn defer(&mut self, command: Option<SessionCommand>) -> Result<(), Abort> {
        match command {
            Some(SessionCommand::Disconnect) | None => Err(Abort::Cancelled),
            Some(command) => {
                self.backlog.push_back(command);
                Ok(())
            }
        }
    }
}

/// 连接目标的配置，以及它是否在目录里（只有目录里的连接能存密码）。
pub struct Target {
    pub profile: ConnectionProfile,
    pub saved: bool,
}

impl Target {
    /// 状态与提示里显示的目标。
    pub fn describe(&self) -> String {
        format!(
            "{}@{}:{}",
            self.profile.username, self.profile.host, self.profile.port
        )
    }
}

/// 找到连接配置：目录里的按 id 取；快速连接由请求里的字段临时组成（密码认证）。
pub async fn resolve_target(
    context: &Arc<AppContext>,
    request: &ConnectRequest,
) -> Result<Target, Abort> {
    if request.connection_id.is_empty() {
        let (host, username) =
            catalog::validate_target(&request.host, request.port, &request.username)
                .map_err(|error| Abort::failed(FailureKind::InvalidTarget, format!("{error:?}")))?;
        let mut profile = ConnectionProfile::new(format!("{username}@{host}"), host);
        profile.port = request.port;
        profile.username = username.to_owned();
        profile.transport = TransportKind::NativeSsh;
        profile.authentication = AuthenticationKind::Password;
        profile.remote_command =
            Some(request.command.trim().to_owned()).filter(|command| !command.is_empty());
        return Ok(Target {
            profile,
            saved: false,
        });
    }

    let id = uuid::Uuid::parse_str(&request.connection_id)
        .map(ConnectionId::from)
        .map_err(|_| Abort::failed(FailureKind::NotFound, request.connection_id.clone()))?;
    let context = context.clone();
    let catalog = spawn_blocking(move || context.repository.load_catalog())
        .await
        .map_err(|error| Abort::failed(FailureKind::Other, format!("catalog task: {error}")))?
        .map_err(|error| Abort::failed(FailureKind::Other, format!("catalog: {error:?}")))?;
    let profile = catalog
        .connections
        .get(&id)
        .cloned()
        .ok_or_else(|| Abort::failed(FailureKind::NotFound, request.connection_id.clone()))?;
    Ok(Target {
        profile,
        saved: true,
    })
}

/// 建立连接。`quick_password` 是快速连接随请求带来的密码（可能为空）。
pub async fn establish(
    context: &Arc<AppContext>,
    session_id: u32,
    target: Target,
    quick_password: SecretString,
    size: TerminalSize,
    mut channels: Channels<'_>,
) -> Result<Connected, Abort> {
    let mut prompts = PromptIds::default();
    let Target { profile, saved } = target;

    // 密码：目录里存了就从钥匙串读（每次连接读，不缓存——PLAN §9.2.2）；
    // 没存、读不到、或快速连接没带，就问用户。
    let mut remember = None;
    let secret = match profile.authentication {
        AuthenticationKind::Password => {
            let password = match saved_password(context, &profile).await {
                Some(password) => password,
                None if !quick_password.expose_secret().is_empty() => quick_password,
                None => {
                    let (password, keep) =
                        ask_password(session_id, prompts.next(), &profile, saved, &mut channels)
                            .await?;
                    if keep && saved {
                        remember = Some(SecretString::from(password.expose_secret().to_owned()));
                    }
                    password
                }
            };
            Some(password)
        }
        _ => None,
    };
    let auth = AuthPlan::from_secret(&profile, secret)
        .map_err(|error| Abort::failed(FailureKind::Other, format!("auth plan: {error:?}")))?;

    // 主机密钥：新主机与变更的密钥都问用户（变更时带 changed，Dart 给出醒目警告）。
    let verifier = KnownHostsVerifier::new(&context.known_hosts).with_changed_key_prompt();
    let mut transport =
        NativeSshTransport::new(profile.clone(), auth, verifier).map_err(|error| {
            Abort::failed(
                failure_kind(error.failure()),
                format!("transport: {error:?}"),
            )
        })?;

    let (broker, mut requests) = interaction_channel();
    let transport_request = TransportRequest::new(size);
    let result = {
        let connect = transport.connect(&transport_request, broker.clone());
        tokio::pin!(connect);
        let mut pending = HashMap::new();
        loop {
            tokio::select! {
                result = &mut connect => break result,
                request = requests.recv() => {
                    if let Some((id, request)) = request {
                        forward_prompt(session_id, &profile, &broker, &mut prompts, &mut pending, id, request);
                    }
                }
                reply = channels.replies.recv() => {
                    if let Some(reply) = reply
                        && let Some((id, kind)) = pending.remove(&reply.prompt_id)
                    {
                        let _ = broker.respond(id, broker_response(kind, reply));
                    }
                }
                command = channels.commands.recv() => channels.defer(command)?,
            }
        }
    };
    if let Err(error) = result {
        debug_print!("[connect] {error:?}");
        return Err(Abort::failed(
            failure_kind(error.failure()),
            format!("connect: {error:?}"),
        ));
    }

    if let Some(password) = remember {
        store_password(context, profile.id, password).await;
    }
    Ok(Connected { transport, profile })
}

/// 本会话内递增的问题编号。
#[derive(Default)]
struct PromptIds(u32);

impl PromptIds {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
}

async fn saved_password(
    context: &Arc<AppContext>,
    profile: &ConnectionProfile,
) -> Option<SecretString> {
    let reference = profile.credential_ref.clone()?;
    let context = context.clone();
    match spawn_blocking(move || context.credentials.get(&reference)).await {
        Ok(Ok(password)) => password,
        Ok(Err(error)) => {
            // 读不到（钥匙串拒绝等）就改为询问，不让连接直接失败。
            debug_print!("[connect] keychain read: {error:?}");
            None
        }
        Err(error) => {
            debug_print!("[connect] keychain task: {error}");
            None
        }
    }
}

/// 问密码。返回密码与「连接成功后存进钥匙串」。
async fn ask_password(
    session_id: u32,
    prompt_id: u32,
    profile: &ConnectionProfile,
    saved: bool,
    channels: &mut Channels<'_>,
) -> Result<(SecretString, bool), Abort> {
    InteractionPrompt {
        can_remember: saved,
        ..empty_prompt(session_id, prompt_id, PromptKind::Password, profile)
    }
    .send_signal_to_dart();
    loop {
        tokio::select! {
            reply = channels.replies.recv() => {
                let Some(reply) = reply else { return Err(Abort::Cancelled) };
                if reply.prompt_id != prompt_id {
                    continue;
                }
                if !reply.accept {
                    return Err(Abort::Cancelled);
                }
                let password = reply.answers.into_iter().next().unwrap_or_default();
                return Ok((SecretString::from(password), reply.remember));
            }
            command = channels.commands.recv() => channels.defer(command)?,
        }
    }
}

/// 上游 broker 的问题 → Dart。上游只会问主机密钥与 keyboard-interactive；
/// 其他种类（不会出现）直接取消。keyboard-interactive 里什么都不用填、也没有
/// 说明文字的一轮（PAM 认证通过后常见的收尾请求）直接回空答案，不打扰用户——
/// 与 OpenSSH 客户端的做法一致。
fn forward_prompt(
    session_id: u32,
    profile: &ConnectionProfile,
    broker: &InteractionBroker,
    prompts: &mut PromptIds,
    pending: &mut HashMap<u32, (InteractionId, PromptKind)>,
    id: InteractionId,
    request: InteractionRequest,
) {
    let prompt_id = prompts.next();
    let prompt = match request {
        InteractionRequest::HostKey(host_key) => {
            host_key_prompt(session_id, prompt_id, profile, host_key)
        }
        InteractionRequest::KeyboardInteractive(questions) if is_empty_round(&questions) => {
            let _ = broker.respond(id, InteractionResponse::Answers(Vec::new()));
            return;
        }
        InteractionRequest::KeyboardInteractive(questions) => {
            keyboard_interactive_prompt(session_id, prompt_id, profile, questions)
        }
        InteractionRequest::Password(_) | InteractionRequest::PrivateKeyPassphrase(_) => {
            let _ = broker.respond(id, InteractionResponse::Cancel);
            return;
        }
    };
    pending.insert(prompt_id, (id, prompt.kind));
    prompt.send_signal_to_dart();
}

fn is_empty_round(questions: &KeyboardInteractivePrompt) -> bool {
    questions.prompts.is_empty()
        && questions.name.trim().is_empty()
        && questions.instruction.trim().is_empty()
}

fn empty_prompt(
    session_id: u32,
    prompt_id: u32,
    kind: PromptKind,
    profile: &ConnectionProfile,
) -> InteractionPrompt {
    InteractionPrompt {
        session_id,
        prompt_id,
        kind,
        username: profile.username.clone(),
        host: profile.host.clone(),
        port: profile.port,
        address: String::new(),
        algorithm: String::new(),
        fingerprint: String::new(),
        changed: false,
        name: String::new(),
        instruction: String::new(),
        fields: Vec::new(),
        can_remember: false,
    }
}

fn host_key_prompt(
    session_id: u32,
    prompt_id: u32,
    profile: &ConnectionProfile,
    host_key: HostKeyPrompt,
) -> InteractionPrompt {
    InteractionPrompt {
        address: host_key.host,
        algorithm: host_key.algorithm,
        fingerprint: host_key.sha256,
        changed: host_key.changed,
        ..empty_prompt(session_id, prompt_id, PromptKind::HostKey, profile)
    }
}

fn keyboard_interactive_prompt(
    session_id: u32,
    prompt_id: u32,
    profile: &ConnectionProfile,
    questions: KeyboardInteractivePrompt,
) -> InteractionPrompt {
    InteractionPrompt {
        name: questions.name,
        instruction: questions.instruction,
        fields: questions
            .prompts
            .into_iter()
            .map(|prompt| PromptField {
                label: prompt.label,
                echo: prompt.echo,
            })
            .collect(),
        ..empty_prompt(
            session_id,
            prompt_id,
            PromptKind::KeyboardInteractive,
            profile,
        )
    }
}

/// Dart 的回答 → 上游 broker 的回答。拒绝主机密钥是 Reject（连接以
/// HostKeyRejected / HostKeyChanged 失败），其余的「不答」是 Cancel。
fn broker_response(kind: PromptKind, reply: InteractionReply) -> InteractionResponse {
    match (kind, reply.accept) {
        (PromptKind::HostKey, true) => {
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore)
        }
        (PromptKind::HostKey, false) => InteractionResponse::HostKey(HostKeyDecision::Reject),
        (PromptKind::KeyboardInteractive, true) => InteractionResponse::Answers(
            reply.answers.into_iter().map(SecretString::from).collect(),
        ),
        (PromptKind::KeyboardInteractive | PromptKind::Password, _) => InteractionResponse::Cancel,
    }
}

/// 连接成功后把用户输入的密码存进钥匙串（目录与钥匙串的一致性由上游
/// `CredentialCoordinator` 保证）。存不进去不影响本次会话。
async fn store_password(context: &Arc<AppContext>, id: ConnectionId, password: SecretString) {
    let task_context = context.clone();
    let stored = spawn_blocking(move || {
        let catalog = task_context
            .repository
            .load_catalog()
            .map_err(|error| format!("{error:?}"))?;
        // 取目录里的当前版本：连接期间用户可能刚改过这条连接。
        let Some(profile) = catalog.connections.get(&id).cloned() else {
            return Ok(false);
        };
        task_context
            .credentials
            .apply_catalog(
                CatalogMutation::Update(profile),
                SecretUpdate::Set(password),
            )
            .map(|_| true)
            .map_err(|error| format!("{error:?}"))
    })
    .await;
    match stored {
        Ok(Ok(true)) => context.catalog_changed.notify_one(),
        Ok(Ok(false)) => {}
        Ok(Err(error)) => debug_print!("[connect] saving password: {error}"),
        Err(error) => debug_print!("[connect] saving password task: {error}"),
    }
}

/// 上游的失败分类 → 边界上的分类。
pub fn failure_kind(failure: SessionFailure) -> FailureKind {
    match failure {
        SessionFailure::Authentication => FailureKind::Authentication,
        SessionFailure::HostKeyRejected => FailureKind::HostKeyRejected,
        SessionFailure::HostKeyChanged => FailureKind::HostKeyChanged,
        SessionFailure::Network => FailureKind::Network,
        SessionFailure::Timeout => FailureKind::Timeout,
        SessionFailure::Vault => FailureKind::Keychain,
        _ => FailureKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::{PromptKind, broker_response, is_empty_round};
    use crate::signals::interaction::InteractionReply;
    use rshell_m0::rshell_core::{
        AuthPrompt, HostKeyDecision, InteractionId, InteractionResponse, KeyboardInteractivePrompt,
    };
    use secrecy::ExposeSecret;

    #[test]
    fn keyboard_interactive_rounds_without_questions_or_text_are_answered_silently() {
        let round = |name: &str, instruction: &str, prompts: usize| KeyboardInteractivePrompt {
            id: InteractionId::new(),
            name: name.to_owned(),
            instruction: instruction.to_owned(),
            prompts: (0..prompts)
                .map(|_| AuthPrompt {
                    id: InteractionId::new(),
                    label: "Password: ".to_owned(),
                    echo: false,
                })
                .collect(),
        };
        assert!(is_empty_round(&round("", " ", 0)));
        assert!(!is_empty_round(&round("", "", 1)));
        assert!(!is_empty_round(&round("", "Your password expires soon", 0)));
    }

    fn reply(accept: bool, answers: &[&str]) -> InteractionReply {
        InteractionReply {
            session_id: 1,
            prompt_id: 1,
            accept,
            answers: answers.iter().map(|answer| (*answer).to_owned()).collect(),
            remember: false,
        }
    }

    #[test]
    fn host_key_answers_accept_or_reject() {
        assert!(matches!(
            broker_response(PromptKind::HostKey, reply(true, &[])),
            InteractionResponse::HostKey(HostKeyDecision::AcceptAndStore)
        ));
        assert!(matches!(
            broker_response(PromptKind::HostKey, reply(false, &[])),
            InteractionResponse::HostKey(HostKeyDecision::Reject)
        ));
    }

    #[test]
    fn keyboard_interactive_answers_keep_their_order() {
        let InteractionResponse::Answers(answers) = broker_response(
            PromptKind::KeyboardInteractive,
            reply(true, &["first", "second"]),
        ) else {
            panic!("expected answers");
        };
        let answers: Vec<&str> = answers
            .iter()
            .map(|answer| answer.expose_secret())
            .collect();
        assert_eq!(answers, ["first", "second"]);
        assert!(matches!(
            broker_response(PromptKind::KeyboardInteractive, reply(false, &["x"])),
            InteractionResponse::Cancel
        ));
    }
}
