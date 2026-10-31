mod fs;
mod tofu;

use std::path::Path;

#[tauri::command]
fn list_dir(path: String) -> Result<Vec<fs::DirEntry>, String> {
    fs::list_dir(Path::new(&path))
}

#[tauri::command]
fn is_module(path: String) -> bool {
    fs::is_module(Path::new(&path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(tofu::Runs::default())
        .setup(|app| {
            tofu::clear_stale_plans(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_dir,
            is_module,
            tofu::run_tofu,
            tofu::apply_plan,
            tofu::discard_plan,
            tofu::cancel_tofu,
            tofu::tofu_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
