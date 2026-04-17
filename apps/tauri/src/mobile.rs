//! Mobile entry point for CclawCore Desktop (iOS/Android).

#[tauri::mobile_entry_point]
fn main() {
    cclawcore_desktop::run();
}
