//! Waits for something to change in the photo folders, so Muninn only rescans
//! when there's something new instead of walking every folder on a timer.

use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::WaitForMultipleObjects;

/// Changes keep coming while a folder is copied in: wait until it's quiet.
const SETTLE: Duration = Duration::from_secs(3);

pub struct Watch(Vec<HANDLE>);

impl Watch {
    /// Start watching. Changes from here on wake `wait`, including ones made
    /// while a scan is still running.
    pub fn new(roots: &[PathBuf]) -> Watch {
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;
        Watch(roots.iter().filter_map(|r| unsafe { FindFirstChangeNotificationW(&HSTRING::from(r.as_os_str()), true, filter).ok() }).collect())
    }

    /// Block until something changed and then settled, or until `max` passed.
    /// Returns whether something changed.
    pub fn wait(&self, max: Duration) -> bool {
        if self.0.is_empty() {
            std::thread::sleep(max);
            return false;
        }
        let start = Instant::now();
        let ms = |d: Duration| d.as_millis().min(u32::MAX as u128 - 1) as u32;
        if !self.next(ms(max)) {
            return false;
        }
        // Something changed: wait for a quiet moment before rescanning.
        while start.elapsed() < max + SETTLE * 4 && self.next(ms(SETTLE)) {}
        true
    }

    /// Wait up to `ms` for a change. Rearms the handle that fired.
    fn next(&self, ms: u32) -> bool {
        let r = unsafe { WaitForMultipleObjects(&self.0, false, ms) };
        if r == WAIT_TIMEOUT {
            return false;
        }
        // WAIT_OBJECT_0 is 0, so the result is the index of the handle that fired.
        match self.0.get(r.0 as usize) {
            Some(&h) => unsafe { FindNextChangeNotification(h).is_ok() },
            // Failed or abandoned: treat as a change, and don't spin.
            None => {
                std::thread::sleep(SETTLE);
                true
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        for &h in &self.0 {
            unsafe { FindCloseChangeNotification(h).ok() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakes_on_new_file_and_times_out_otherwise() {
        let dir = std::env::temp_dir().join(format!("muninn-watch-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let w = Watch::new(&[dir.clone()]);
        let t = Instant::now();
        w.wait(Duration::from_millis(400));
        assert!(t.elapsed() >= Duration::from_millis(390), "returned early without a change");

        let w = Watch::new(&[dir.clone()]);
        let d2 = dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(d2.join("sub").join("new.jpg"), b"x").unwrap();
        });
        let t = Instant::now();
        w.wait(Duration::from_secs(30));
        let took = t.elapsed();
        std::fs::remove_dir_all(&dir).ok();
        // The change at ~0.3 s, then the 3 s settle.
        assert!(took > Duration::from_secs(3) && took < Duration::from_secs(8), "took {took:?}");
    }
}
