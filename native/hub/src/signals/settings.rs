//! 设置（上游默认 `TerminalProfile` 的字体与字号）。

use rinf::{DartSignal, RustSignal};
use serde::{Deserialize, Serialize};

/// 取当前设置。
#[derive(Deserialize, DartSignal)]
pub struct SettingsQuery {}

#[derive(Deserialize, DartSignal)]
pub struct SaveSettings {
    pub font_family: String,
    pub font_size: f64,
}

/// 当前设置。设置变化后重发。
#[derive(Serialize, RustSignal)]
pub struct SettingsState {
    pub font_family: String,
    pub font_size: f64,
    /// 可选的字体（App 内置或系统自带）。
    pub font_families: Vec<String>,
    pub min_font_size: f64,
    pub max_font_size: f64,
}
