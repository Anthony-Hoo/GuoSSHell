//! 设置：上游默认 `TerminalProfile` 里的字体与字号（引擎也从这份配置取
//! scrollback 等终端参数）。

use std::sync::Arc;

use rinf::{DartSignal, RustSignal, debug_print};
use rshell_m0::rshell_core::{TerminalProfile, TerminalSettingsV1};
use rshell_m0::rshell_storage::{SqliteRepository, StorageError};
use tokio::task::spawn_blocking;

use crate::app::AppContext;
use crate::signals::settings::{SaveSettings, SettingsQuery, SettingsState};

/// 可选字体：第一个随 App 内置（带 powerline / Nerd Font 字形），其余是系统自带。
pub const FONT_FAMILIES: [&str; 2] = ["MesloLGS NF", "Menlo"];
const DEFAULT_FONT_SIZE: f32 = 14.0;
const MIN_FONT_SIZE: f32 = 8.0;
const MAX_FONT_SIZE: f32 = 32.0;

/// 默认终端配置（设置里指定的那份；找不到就用上游内置默认）。
pub fn default_profile(repository: &SqliteRepository) -> Result<TerminalProfile, StorageError> {
    let settings = repository.load_settings()?;
    Ok(repository
        .load_terminal_profiles()?
        .into_iter()
        .find(|profile| profile.id == settings.default_terminal_profile)
        .unwrap_or_else(TerminalProfile::p0_default))
}

/// 默认配置里的字体不是本 App 提供的（上游迁移种下的默认值就是这样）时，
/// 换成本 App 的默认字体与字号。
pub fn adopt_app_defaults(repository: &SqliteRepository) -> Result<(), String> {
    let mut profile =
        default_profile(repository).map_err(|error| format!("load settings: {error:?}"))?;
    if FONT_FAMILIES.contains(&profile.settings.font_family.as_str()) {
        return Ok(());
    }
    profile.settings.font_family = FONT_FAMILIES[0].to_owned();
    profile.settings.font_size = DEFAULT_FONT_SIZE;
    repository
        .save_terminal_profile(profile)
        .map_err(|error| format!("save settings: {error:?}"))
}

/// 新会话用的终端配置。读不到就用上游内置默认（不让会话因此失败）。
pub async fn terminal_settings(context: &Arc<AppContext>) -> TerminalSettingsV1 {
    let context = context.clone();
    match spawn_blocking(move || default_profile(&context.repository)).await {
        Ok(Ok(profile)) => profile.settings,
        Ok(Err(error)) => {
            debug_print!("[settings] load: {error:?}");
            TerminalSettingsV1::default()
        }
        Err(error) => {
            debug_print!("[settings] load task: {error}");
            TerminalSettingsV1::default()
        }
    }
}

pub async fn run(context: Arc<AppContext>) {
    let query_rx = SettingsQuery::get_dart_signal_receiver();
    let save_rx = SaveSettings::get_dart_signal_receiver();
    loop {
        tokio::select! {
            pack = query_rx.recv() => {
                if pack.is_none() {
                    break;
                }
            }
            pack = save_rx.recv() => {
                let Some(pack) = pack else { break };
                let request = pack.message;
                let task_context = context.clone();
                let saved = spawn_blocking(move || save(&task_context.repository, &request)).await;
                match saved {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => debug_print!("[settings] save: {error:?}"),
                    Err(error) => debug_print!("[settings] save task: {error}"),
                }
            }
        }
        publish(&context).await;
    }
}

/// 存设置。字体必须是可选字体之一（否则不改），字号夹到允许范围。
fn save(repository: &SqliteRepository, request: &SaveSettings) -> Result<(), StorageError> {
    let mut profile = default_profile(repository)?;
    if let Some(family) = FONT_FAMILIES
        .iter()
        .find(|family| **family == request.font_family)
    {
        profile.settings.font_family = (*family).to_owned();
    }
    if request.font_size.is_finite() {
        profile.settings.font_size = (request.font_size as f32).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
    }
    repository.save_terminal_profile(profile)
}

async fn publish(context: &Arc<AppContext>) {
    let settings = terminal_settings(context).await;
    SettingsState {
        font_family: settings.font_family,
        font_size: f64::from(settings.font_size),
        font_families: FONT_FAMILIES
            .iter()
            .map(|family| (*family).to_owned())
            .collect(),
        min_font_size: f64::from(MIN_FONT_SIZE),
        max_font_size: f64::from(MAX_FONT_SIZE),
    }
    .send_signal_to_dart();
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{FONT_FAMILIES, adopt_app_defaults, default_profile, save};
    use crate::signals::settings::SaveSettings;
    use rshell_m0::rshell_storage::SqliteRepository;

    fn repository() -> SqliteRepository {
        let repository = SqliteRepository::open_in_memory().expect("in-memory catalog");
        repository.migrate().expect("migrate");
        repository
    }

    #[test]
    fn upstream_seed_is_replaced_by_the_app_defaults_once() {
        let repository = repository();
        adopt_app_defaults(&repository).expect("adopt defaults");
        let profile = default_profile(&repository).expect("profile");
        assert_eq!(profile.settings.font_family, FONT_FAMILIES[0]);
        assert_eq!(profile.settings.font_size, 14.0);

        save(
            &repository,
            &SaveSettings {
                font_family: "Menlo".to_owned(),
                font_size: 17.0,
            },
        )
        .expect("save");
        adopt_app_defaults(&repository).expect("adopt defaults again");
        let profile = default_profile(&repository).expect("profile");
        assert_eq!(profile.settings.font_family, "Menlo");
        assert_eq!(profile.settings.font_size, 17.0);
    }

    #[test]
    fn unknown_fonts_are_ignored_and_sizes_clamped() {
        let repository = repository();
        adopt_app_defaults(&repository).expect("adopt defaults");
        save(
            &repository,
            &SaveSettings {
                font_family: "Comic Sans".to_owned(),
                font_size: 200.0,
            },
        )
        .expect("save");
        let profile = default_profile(&repository).expect("profile");
        assert_eq!(profile.settings.font_family, FONT_FAMILIES[0]);
        assert_eq!(profile.settings.font_size, 32.0);
    }
}
