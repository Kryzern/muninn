//! Settings, kept as JSON in %LOCALAPPDATA%\Muninn\settings.json. Every
//! option changes with one click (or Enter) on the Settings tab.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    /// Lots of photos drifting in the background.
    Full,
    /// Fewer, further back.
    Quiet,
    /// A plain background.
    Off,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    Auto,
    Cpu,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Folders to read. Empty means Pictures, Desktop, Downloads and OneDrive Pictures.
    pub folders: Vec<String>,
    /// The model in use, an id from models.json. Empty until one is installed.
    pub model: String,
    /// Read text inside photos (receipts, screenshots, signs).
    pub read_text: bool,
    /// Leave photos that are only in the cloud alone instead of downloading them.
    pub skip_cloud: bool,
    pub glass: bool,
    pub animations: bool,
    pub field: Field,
    pub start_with_windows: bool,
    pub check_updates: bool,
    pub device: Device,
    /// Last window position and size, in physical pixels.
    pub window: Option<[i32; 4]>,
    pub maximized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            folders: vec![],
            model: String::new(),
            read_text: true,
            skip_cloud: true,
            glass: true,
            animations: true,
            field: Field::Full,
            start_with_windows: false,
            check_updates: true,
            device: Device::Auto,
            window: None,
            maximized: false,
        }
    }
}

fn path() -> PathBuf {
    crate::index::data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Settings {
        let mut s: Settings = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // 0.3 had no settings: the check was turned off with an empty file
        if crate::index::data_dir().join("no-updates").exists() {
            s.check_updates = false;
        }
        s
    }

    pub fn save(&self) {
        if let Ok(b) = serde_json::to_vec_pretty(self) {
            let tmp = path().with_extension("json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(tmp, path());
            }
        }
    }

    /// The folders to read, with the defaults filled in.
    pub fn roots(&self) -> Vec<PathBuf> {
        if self.folders.is_empty() {
            return crate::index::default_roots();
        }
        let mut r: Vec<PathBuf> = self.folders.iter().map(PathBuf::from).filter(|p| p.exists()).collect();
        r.dedup();
        r
    }
}

/// Open Muninn at sign-in, in the background so it can keep the index fresh.
pub fn set_autostart(on: bool) {
    use windows::core::{w, HSTRING};
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, w!(r"Software\Microsoft\Windows\CurrentVersion\Run"), None, KEY_SET_VALUE, &mut key).is_err() {
            return;
        }
        if on {
            if let Ok(exe) = std::env::current_exe() {
                let v: Vec<u16> = format!("\"{}\" --minimized", exe.display()).encode_utf16().chain(Some(0)).collect();
                let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
                let _ = RegSetValueExW(key, w!("Muninn"), None, REG_SZ, Some(bytes));
            }
        } else {
            let _ = RegDeleteValueW(key, &HSTRING::from("Muninn"));
        }
        let _ = RegCloseKey(key);
    }
}
