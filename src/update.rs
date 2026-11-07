//! Update checks against GitHub releases. Muninn only says a new version is
//! out; the setup (a few hundred MB, the model is inside it) is downloaded
//! when you click Update, then run silently, and it starts Muninn again.
//! Copies that weren't installed by the setup get a link to the release.
//! Creating `%LOCALAPPDATA%\Muninn\no-updates` turns the check off.

use std::io::Write;
use std::path::{Path, PathBuf};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;

const LATEST: &str = "https://api.github.com/repos/Kryzern/muninn/releases/latest";

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    /// The release's setup.exe, if it has one.
    pub setup: Option<String>,
    pub page: String,
}


/// Was this copy installed by the setup, so the next setup can go over it?
pub fn self_updates() -> bool {
    std::env::current_exe().is_ok_and(|e| e.with_file_name("unins000.exe").exists())
}

/// The latest release, if it's newer than this copy.
pub fn check() -> Option<Release> {
    let r = latest()?;
    newer(&r.version, env!("CARGO_PKG_VERSION")).then_some(r)
}

/// Download the release's setup, reporting progress from 0 to 1.
pub fn download(r: &Release, progress: impl FnMut(f32)) -> Option<PathBuf> {
    let url = r.setup.as_deref()?;
    let dir = std::env::temp_dir().join("Muninn-update");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("Muninn-{}-x64-setup.exe", r.version));
    let part = path.with_extension("part");
    let n = get_to(url, &part, progress)?;
    let mut head = [0u8; 2];
    let ok = n > 1_000_000 && std::fs::File::open(&part).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head == b"MZ";
    if !ok {
        std::fs::remove_file(&part).ok();
        return None;
    }
    std::fs::rename(&part, &path).ok()?;
    Some(path)
}

/// Start the setup. It closes Muninn, installs over it and opens it again.
pub fn install(setup: &Path) -> bool {
    std::process::Command::new(setup)
        .args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS"])
        .spawn()
        .is_ok()
}

/// Is version `a` ("1.2.0" or "v1.2.0") later than `b`?
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.trim().trim_start_matches('v').split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    parse(a) > parse(b)
}

fn latest() -> Option<Release> {
    let mut body = Vec::new();
    request(LATEST, |chunk, _| {
        body.extend_from_slice(chunk);
        true
    })?;
    let v: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let version = v["tag_name"].as_str()?.trim_start_matches('v').to_string();
    let setup = v["assets"].as_array()?.iter().find_map(|a| {
        let name = a["name"].as_str()?;
        name.ends_with("-x64-setup.exe").then(|| a["browser_download_url"].as_str().map(str::to_string))?
    });
    let page = v["html_url"].as_str().unwrap_or("https://github.com/Kryzern/muninn/releases/latest").to_string();
    Some(Release { version, setup, page })
}

/// Stream a download into a file, reporting (bytes so far, total). `go` is
/// asked before every chunk and stops the download when it says no.
pub fn fetch(url: &str, path: &Path, mut progress: impl FnMut(u64, u64), go: impl Fn() -> bool) -> Option<u64> {
    let mut f = std::fs::File::create(path).ok()?;
    let mut got = 0u64;
    request(url, |chunk, total| {
        if !go() {
            return false;
        }
        got += chunk.len() as u64;
        progress(got, total);
        f.write_all(chunk).is_ok()
    })?;
    Some(got)
}

fn get_to(url: &str, path: &Path, mut progress: impl FnMut(f32)) -> Option<u64> {
    let mut f = std::fs::File::create(path).ok()?;
    let mut got = 0u64;
    request(url, |chunk, total| {
        got += chunk.len() as u64;
        if total > 0 {
            progress(got as f32 / total as f32);
        }
        f.write_all(chunk).is_ok()
    })?;
    Some(got)
}

/// HTTP(S) GET, following redirects, handing the body over in chunks along with
/// the total size (0 if unknown). None on any failure or a non-200 status.
fn request(url: &str, mut sink: impl FnMut(&[u8], u64) -> bool) -> Option<()> {
    struct H(*mut core::ffi::c_void);
    impl Drop for H {
        fn drop(&mut self) {
            unsafe { WinHttpCloseHandle(self.0).ok() };
        }
    }
    let (secure, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, url.strip_prefix("http://")?),
    };
    let (hostport, path) = rest.split_at(rest.find('/')?);
    let (host, port) = match hostport.split_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, if secure { INTERNET_DEFAULT_HTTPS_PORT } else { INTERNET_DEFAULT_HTTP_PORT }),
    };
    let agent = HSTRING::from(format!("Muninn/{}", env!("CARGO_PKG_VERSION")));
    unsafe {
        let session = H(WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0));
        if session.0.is_null() {
            return None;
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000).ok();
        let conn = H(WinHttpConnect(session.0, &HSTRING::from(host), port, 0));
        if conn.0.is_null() {
            return None;
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let req = H(WinHttpOpenRequest(conn.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags));
        if req.0.is_null() {
            return None;
        }
        WinHttpSendRequest(req.0, None, None, 0, 0, 0).ok()?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut()).ok()?;
        let query = |flag: u32| -> u32 {
            let (mut v, mut len) = (0u32, 4u32);
            let ok = WinHttpQueryHeaders(req.0, flag | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some(&mut v as *mut u32 as *mut _), &mut len, std::ptr::null_mut());
            if ok.is_ok() { v } else { 0 }
        };
        if query(WINHTTP_QUERY_STATUS_CODE) != 200 {
            return None;
        }
        let total = query(WINHTTP_QUERY_CONTENT_LENGTH) as u64;
        let mut buf = vec![0u8; 256 << 10];
        loop {
            let mut read = 0u32;
            WinHttpReadData(req.0, buf.as_mut_ptr() as *mut _, buf.len() as u32, &mut read).ok()?;
            if read == 0 {
                return Some(());
            }
            if !sink(&buf[..read as usize], total) {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn versions() {
        assert!(newer("v0.4.0", "0.3.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.3.0", "0.3.0"));
        assert!(!newer("v0.2.9", "0.3.0"));
    }
}

#[cfg(test)]
mod live {
    /// Streams a real release asset (Heimdall's, until Muninn has releases).
    #[test]
    #[ignore]
    fn download_streams_to_file() {
        let mut body = Vec::new();
        super::request("https://api.github.com/repos/Kryzern/Heimdall/releases/latest", |c, _| { body.extend_from_slice(c); true }).expect("reach GitHub");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let url = v["assets"].as_array().unwrap().iter().find_map(|a| a["browser_download_url"].as_str().filter(|u| u.ends_with("-setup.exe"))).unwrap().to_string();
        let r = super::Release { version: "test".into(), setup: Some(url), page: String::new() };
        let mut last = 0.0;
        let path = super::download(&r, |f| last = f).expect("download");
        eprintln!("{} bytes, last progress {last}", std::fs::metadata(&path).unwrap().len());
        assert!(last > 0.99);
        std::fs::remove_file(path).ok();
    }

    #[test]
    #[ignore]
    fn muninn_repo_has_no_release_yet() {
        eprintln!("check() = {:?}", super::check().map(|r| r.version));
    }
}
