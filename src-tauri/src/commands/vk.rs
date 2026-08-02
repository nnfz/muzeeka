// VK auth Tauri commands.

use tauri::AppHandle;

#[tauri::command]
pub fn vk_auth_status(app: AppHandle) -> crate::vk_audio::VkAuthStatus {
    crate::vk_audio::auth_status(&app)
}

/// Open VK login window and wait until the user signs in.
#[tauri::command]
pub async fn vk_login(app: AppHandle) -> Result<crate::vk_audio::VkAuthStatus, String> {
    crate::vk_audio::login(app).await
}

/// Clear saved VK session.
#[tauri::command]
pub async fn vk_logout(app: AppHandle) -> Result<crate::vk_audio::VkAuthStatus, String> {
    crate::vk_audio::logout(app).await
}
