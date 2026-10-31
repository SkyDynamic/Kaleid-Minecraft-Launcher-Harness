//! 「使用高性能显卡」：写 Windows 的 GPU 首选项。
//!
//! 这件事只能做在注册表里，没有别的公开接口：
//! `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` 下，**值名是可执行文件的
//! 完整路径**，值是 `GpuPreference=2;`（2 = 高性能 GPU，1 = 省电 GPU，0 = 系统决定）。
//!
//! 两条纪律：
//! - 只写 HKCU，不碰 HKLM（不需要管理员权限，也不会影响别的用户）；
//! - 失败只当**警告**：写不进去不能拦住启动（老系统 / 组策略锁住都可能失败）。

use std::path::Path;

/// GPU 首选项的注册表键（HKCU 下）。
pub const GPU_PREF_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
/// 「高性能」的取值。
pub const GPU_PREF_HIGH_PERFORMANCE: &str = "GpuPreference=2;";

/// 把某个可执行文件设成"使用高性能显卡"。
#[cfg(windows)]
pub fn set_high_performance(exe: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let path_text = exe.to_string_lossy().to_string();
    if path_text.trim().is_empty() {
        return Err("可执行文件路径是空的".to_string());
    }
    if !exe.is_file() {
        return Err(format!("找不到可执行文件：{path_text}"));
    }

    // 值名必须是完整路径原文（Windows 按这个字符串匹配进程）
    let value_name: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let key_name = wide(GPU_PREF_KEY);
    // windows-sys 0.52 里 HKEY 就是 isize
    let mut key: isize = 0;
    let mut disposition = 0u32;
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null_mut(),
            &mut key,
            &mut disposition,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!("打不开注册表键（错误码 {status}）"));
    }

    let data = wide(GPU_PREF_HIGH_PERFORMANCE);
    let bytes = unsafe {
        std::slice::from_raw_parts(
            data.as_ptr() as *const u8,
            data.len() * std::mem::size_of::<u16>(),
        )
    };
    let status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS {
        return Err(format!("写注册表失败（错误码 {status}）"));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_high_performance(_exe: &Path) -> Result<(), String> {
    Err("只有 Windows 能设置「使用高性能显卡」".to_string())
}

/// 取消某个可执行文件的首选项（删掉那个值）。
///
/// 关掉开关时要真的删：留在注册表里的话，系统下次启动它仍然优先用独显，
/// 用户会以为"关了没用"。
#[cfg(windows)]
pub fn clear_preference(exe: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY_CURRENT_USER, KEY_SET_VALUE,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let key_name = wide(GPU_PREF_KEY);
    let mut key: isize = 0;
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    if status != ERROR_SUCCESS {
        return Err(format!("打不开注册表键（错误码 {status}）"));
    }
    let value_name: Vec<u16> = exe
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    unsafe {
        RegCloseKey(key);
    }
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("删除注册表值失败（错误码 {status}）"))
    }
}

#[cfg(not(windows))]
pub fn clear_preference(_exe: &Path) -> Result<(), String> {
    Ok(())
}

/// 按开关状态设置/取消**启动器自己**的首选项。
///
/// 开关一改就要生效：等到下次启动游戏才写，用户会觉得「开了没反应」。
pub fn apply_launcher_preference(enabled: bool) -> Vec<String> {
    let mut notes = Vec::new();
    if !cfg!(windows) {
        return notes;
    }
    let Ok(exe) = std::env::current_exe() else {
        notes.push("读不到启动器自身的路径，显卡设置没有生效".to_string());
        return notes;
    };
    let result = if enabled {
        set_high_performance(&exe)
    } else {
        clear_preference(&exe)
    };
    if let Err(e) = result {
        notes.push(format!("显卡设置没有写入注册表：{e}"));
    }
    notes
}

/// 对启动器自己和 Java 都设成高性能显卡，返回要显示给用户的说明。
///
/// 注意：**不是**失败就报错。写不进去（老系统、组策略、绿色版被拒）时给一条
/// 警告就够了——用户仍然能正常玩游戏。
pub fn apply_high_performance(launcher_exe: Option<&Path>, java_exe: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    if !cfg!(windows) {
        notes.push("「使用高性能显卡」只在 Windows 上生效，已跳过".to_string());
        return notes;
    }
    if let Some(exe) = launcher_exe {
        if let Err(e) = set_high_performance(exe) {
            notes.push(format!("设置启动器使用高性能显卡失败：{e}（不影响游戏）"));
        }
    }
    if let Err(e) = set_high_performance(java_exe) {
        notes.push(format!("设置 Java 使用高性能显卡失败：{e}（不影响游戏）"));
    }
    notes
}

/// 当前 Windows 的 build 号（`1809` = 17763）。
///
/// ZGC 在 Windows 上要求 1809+，所以要读真实的 build 号而不是"猜"。
#[cfg(windows)]
pub fn windows_build() -> Option<u32> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_SZ,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let key_name = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let value_name = wide("CurrentBuildNumber");
    let mut key: isize = 0;
    let status =
        unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, key_name.as_ptr(), 0, KEY_QUERY_VALUE, &mut key) };
    if status != ERROR_SUCCESS {
        return None;
    }
    let mut buf = [0u16; 32];
    let mut len = (buf.len() * std::mem::size_of::<u16>()) as u32;
    let mut kind = 0u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut len,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != ERROR_SUCCESS || kind != REG_SZ {
        return None;
    }
    let text = String::from_utf16_lossy(&buf)
        .trim_end_matches('\0')
        .to_string();
    text.parse::<u32>().ok()
}

#[cfg(not(windows))]
pub fn windows_build() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_exe_is_reported_not_panicked() {
        let err = set_high_performance(Path::new("C:/definitely/not/here/java.exe")).unwrap_err();
        // 非 Windows 上会是"只在 Windows 生效"，Windows 上是"找不到可执行文件"
        assert!(!err.is_empty());
    }

    #[test]
    fn empty_path_is_rejected() {
        let err = set_high_performance(Path::new("")).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn gpu_settings_never_block_launch() {
        // 不存在的路径也只是"警告字符串"，不是 Err
        let notes = apply_high_performance(
            Some(Path::new("C:/definitely/not/here/launcher.exe")),
            Path::new("C:/definitely/not/here/javaw.exe"),
        );
        if cfg!(windows) {
            assert_eq!(notes.len(), 2);
            assert!(notes.iter().all(|n| n.contains("不影响游戏")));
        } else {
            assert_eq!(notes.len(), 1);
            assert!(notes[0].contains("只在 Windows"));
        }
    }

    #[test]
    fn windows_build_is_plausible() {
        if cfg!(windows) {
            let build = windows_build().expect("Windows 上应该能读到 build 号");
            assert!(build > 7000, "读到的 build 号不合理：{build}");
        }
    }

    /// 真机往返：写一条、用 `reg query` 读回来、删掉。
    ///
    /// 用 `notepad.exe` 当靶子，不碰 Minecraft/启动器本身，跑完把值删干净。
    /// 默认不跑（会改真实注册表），需要时手动：
    /// `cargo test -p kaleid-core-plugins --lib gpu::tests::real_registry_roundtrip -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore = "会写真实注册表，手动跑"]
    fn real_registry_roundtrip() {
        let exe = std::path::Path::new(r"C:\Windows\System32\notepad.exe");
        if !exe.is_file() {
            return;
        }
        set_high_performance(exe).expect("写入失败");

        let full_key = format!("HKCU\\{GPU_PREF_KEY}");
        let query = || {
            let out = std::process::Command::new("reg")
                .args(["query", &full_key])
                .output()
                .expect("reg query 跑不起来");
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        let has_notepad_pref = |text: &str| {
            text.lines().any(|line| {
                line.contains("notepad.exe")
                    && line.to_ascii_lowercase().contains("reg_sz")
                    && line.contains(GPU_PREF_HIGH_PERFORMANCE)
            })
        };
        let after = query();
        assert!(
            has_notepad_pref(&after),
            "注册表里应当出现 notepad.exe → GpuPreference=2;\n{after}"
        );

        clear_preference(exe).expect("删除失败");
        let after_clear = query();
        assert!(
            !has_notepad_pref(&after_clear),
            "关掉之后值应当被删掉\n{after_clear}"
        );
    }
}
