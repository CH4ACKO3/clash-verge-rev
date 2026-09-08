use super::{CmdResult, StringifyErr as _};
use crate::core::openconnect::{self, OpenConnectDiscovery, OpenConnectSettings, OpenConnectStatus};

#[tauri::command]
pub async fn discover_openconnect() -> CmdResult<OpenConnectDiscovery> {
    openconnect::discover().await.stringify_err()
}

#[tauri::command]
pub async fn install_openconnect() -> CmdResult<OpenConnectDiscovery> {
    openconnect::install().await.stringify_err()
}

#[tauri::command]
pub async fn get_openconnect_settings() -> CmdResult<Option<OpenConnectSettings>> {
    openconnect::load_settings().await.stringify_err()
}

#[tauri::command]
pub async fn save_openconnect_settings(settings: OpenConnectSettings, password: Option<String>) -> CmdResult {
    openconnect::save_settings(&settings, password.as_deref())
        .await
        .stringify_err()
}

#[tauri::command]
pub async fn get_openconnect_status() -> CmdResult<OpenConnectStatus> {
    openconnect::status().await.stringify_err()
}

#[tauri::command]
pub async fn set_openconnect_connected(enabled: bool) -> CmdResult<OpenConnectStatus> {
    openconnect::set_connected(enabled).await.stringify_err()
}
