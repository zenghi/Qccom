//! Read installer locations from the per-user and machine uninstall registries.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::w;
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
};

fn value(root: HKEY, subkey: &[u16], name: *const u16) -> Option<OsString> {
    let mut data = [0u16; 4096];
    let mut bytes = (data.len() * 2) as u32;
    let result = unsafe { RegGetValueW(root, subkey.as_ptr(), name, RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ, null_mut(), data.as_mut_ptr().cast(), &mut bytes) };
    if result != ERROR_SUCCESS {
        return None;
    }
    let end = data.iter().position(|v| *v == 0).unwrap_or(data.len());
    Some(OsString::from_wide(&data[..end]))
}

pub(super) fn directories(names: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
            let mut key = null_mut();
            let result = unsafe { RegOpenKeyExW(root, w!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"), 0, KEY_READ | view, &mut key) };
            if result != ERROR_SUCCESS {
                continue;
            }
            for index in 0.. {
                let mut subkey = [0u16; 256];
                let mut len = (subkey.len() - 1) as u32;
                let result = unsafe { RegEnumKeyExW(key, index, subkey.as_mut_ptr(), &mut len, null(), null_mut(), null_mut(), null_mut()) };
                if result == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if result != ERROR_SUCCESS {
                    continue;
                }
                let Some(name) = value(key, &subkey, w!("DisplayName")) else { continue };
                let lower = name.to_string_lossy().to_ascii_lowercase();
                if !names.iter().any(|pat| lower.contains(pat)) {
                    continue;
                }
                let install = value(key, &subkey, w!("InstallLocation")).map(|value| PathBuf::from(value.to_string_lossy().trim().trim_matches('"')));
                let location = install.filter(|p| p.is_absolute() && p.is_dir()).or_else(|| {
                    let icon = value(key, &subkey, w!("DisplayIcon"))?;
                    let icon = icon.to_string_lossy();
                    let end = icon.to_ascii_lowercase().find(".exe")? + 4;
                    let path = PathBuf::from(icon[..end].trim().trim_start_matches('"'));
                    path.is_absolute().then(|| path.parent().map(|p| p.to_owned())).flatten()
                });
                if let Some(path) = location.filter(|path| path.is_dir() && !out.contains(path)) {
                    out.push(path);
                }
            }
            unsafe { RegCloseKey(key) };
        }
    }
    out
}

pub(super) fn fixed_drives() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = b'A' + i;
            let root = [letter as u16, b':' as u16, b'\\' as u16, 0];
            (unsafe { GetDriveTypeW(root.as_ptr()) } == 3).then(|| PathBuf::from(format!("{}:\\", letter as char)))
        })
        .collect()
}
