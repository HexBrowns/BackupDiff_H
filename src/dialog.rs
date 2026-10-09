//! Windows のファイルのダイアログ（フォルダを選ぶ・CSV の保存先を選ぶ）と、ファイルの日時
//!
//! ダイアログはボタンを押したときに UI のスレッドで開くモーダル。シェルが中で起こすスレッドはこちらから待てないので、
//! 初めて開くときに DLL を pin する（本体が DLL を外した後に残ったスレッドが動いて落ちないように。
//! `.claude/rules/au2-rs-plugin.md`「待てないスレッドが残るなら DLL を pin する」）

use std::path::{Path, PathBuf};
use std::sync::Once;

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{FILETIME, HMODULE, SYSTEMTIME};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileSaveDialog, IFileDialog, IFileOpenDialog, IFileSaveDialog, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT,
    FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

static PIN: Once = Once::new();

fn pin_module() {
    PIN.call_once(|| unsafe {
        let mut module = HMODULE::default();
        let addr = pin_module as *const () as *const u16;
        if let Err(e) = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(addr),
            &mut module,
        ) {
            tracing::warn!("BackupDiff_H: DLL を pin できませんでした: {e}");
        }
    });
}

/// COM を初期化して `f` を呼ぶ。こちらで初期化できたときだけ後始末する
fn with_com<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let out = f();
    if hr.is_ok() {
        unsafe { CoUninitialize() };
    }
    out
}

unsafe fn show_and_get(dialog: &IFileDialog) -> Option<PathBuf> {
    let owner = GetForegroundWindow();
    dialog.Show(Some(owner)).ok()?;
    let item = dialog.GetResult().ok()?;
    let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let path = name.to_string().ok();
    CoTaskMemFree(Some(name.0 as *const _));
    path.map(PathBuf::from)
}

/// フォルダを選ぶ。取り消したら `None`
pub fn pick_folder() -> Option<PathBuf> {
    pin_module();
    with_com(|| unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        let _ = dialog.SetTitle(w!("棚卸しするフォルダ"));
        show_and_get(&dialog.into())
    })
}

/// CSV の保存先を選ぶ。取り消したら `None`
pub fn save_csv(default_name: &str) -> Option<PathBuf> {
    pin_module();
    with_com(|| unsafe {
        let dialog: IFileSaveDialog = CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_OVERWRITEPROMPT | FOS_FORCEFILESYSTEM).ok()?;
        let filter = [COMDLG_FILTERSPEC { pszName: w!("CSV"), pszSpec: w!("*.csv") }];
        dialog.SetFileTypes(&filter).ok()?;
        dialog.SetDefaultExtension(w!("csv")).ok()?;
        let _ = dialog.SetFileName(&HSTRING::from(default_name));
        show_and_get(&dialog.into())
    })
}

/// ファイルの更新日時を、ローカル時刻の `YYYY-MM-DD HH:MM:SS` にする
pub fn modified_text(path: &Path) -> Option<String> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    // FILETIME は 1601-01-01 からの 100ns 単位
    let ticks = d.as_nanos() / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        FileTimeToSystemTime(&ft, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()?;
    }
    Some(format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    ))
}
