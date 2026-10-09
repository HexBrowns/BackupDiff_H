//! BackupDiff_H（バックアップ差分）
//!
//! 本体の自動バックアップ（`Backup/AutoBackup_*.aup2`）を 2 つ選び、オブジェクトを対応付けてから
//! 効果・設定値の違いを一覧にする。仕様は `AI/specifications/20261005_BackupDiff_H_spec.md`。
//!
//! - 比較はボタンを押したときに UI のスレッドで行う（実物で数十 ms。`backups.rs` の `real_backups`）
//! - 本体の API は「比較」で B に編集中のシーンを選んだときの読み取り（`call_read_section`）と、
//!   「戻す」を押したときの書き込み（`call_edit_section`）だけ（`live.rs`）
//! - 項目の履歴と効果の棚卸しは裏のスレッドで作る。スレッドはプラグインの `Drop` で止めて終わりを待つ（`worker.rs`）

mod aup2;
mod backups;
mod dialog;
mod diff;
mod gui;
mod history;
mod inventory;
mod live;
mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use aviutl2::AnyResult;
use aviutl2_eframe::egui;
use parking_lot::{Mutex, RwLock};

pub const WINDOW_NAME: &str = "バックアップ差分";

/// プラグイン本体と UI で共有する状態
#[derive(Default)]
pub struct Shared {
    /// 開いているプロジェクトの `.aup2`。未保存なら `None`
    pub project_path: Option<PathBuf>,
    pub egui_ctx: Option<egui::Context>,
}

pub type SharedState = Arc<RwLock<Shared>>;

fn init_logging() {
    use aviutl2::tracing::Level;
    let level = if cfg!(debug_assertions) { Level::DEBUG } else { Level::INFO };
    let _ = aviutl2::tracing_subscriber::fmt()
        .with_max_level(level)
        .event_format(aviutl2::logger::AviUtl2Formatter)
        .with_writer(aviutl2::logger::AviUtl2LogWriter)
        .try_init();
}

#[aviutl2::plugin(GenericPlugin)]
pub struct BackupDiffPlugin {
    window: Mutex<Option<aviutl2_eframe::EframeWindow>>,
    shared: SharedState,
}

impl BackupDiffPlugin {
    fn set_project_path(&self, path: Option<PathBuf>) {
        let mut s = self.shared.write();
        s.project_path = path;
        if let Some(ctx) = &s.egui_ctx {
            ctx.request_repaint();
        }
    }
}

impl aviutl2::generic::GenericPlugin for BackupDiffPlugin {
    fn new(_info: aviutl2::AviUtl2Info) -> AnyResult<Self> {
        init_logging();
        tracing::info!("BackupDiff_H v{} 初期化", env!("CARGO_PKG_VERSION"));
        Ok(Self { window: Mutex::new(None), shared: Arc::new(RwLock::new(Shared::default())) })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "BackupDiff_H".to_string(),
            information: format!("BackupDiff_H v{} - バックアップ差分 / by HexBrowns", env!("CARGO_PKG_VERSION")),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        live::EDIT_HANDLE.init(registry.create_edit_handle());
        let backup_dir = aviutl2::config::app_data_path().join("Backup");
        let shared = Arc::clone(&self.shared);
        let window = match aviutl2_eframe::EframeWindow::new(WINDOW_NAME, move |cc, handle| {
            Ok(Box::new(gui::BackupDiffApp::new(cc, handle, shared, backup_dir)))
        }) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("バックアップ差分のウィンドウを作れませんでした: {e:#}");
                return;
            }
        };
        match window.handle() {
            Ok(handle) => {
                if let Err(e) = registry.register_window_client(WINDOW_NAME, &handle) {
                    tracing::error!("register_window_client 失敗: {e}");
                }
            }
            Err(e) => tracing::error!("バックアップ差分のウィンドウのハンドルを取れませんでした: {e:#}"),
        }
        *self.window.lock() = Some(window);
    }

    fn on_project_load(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        self.set_project_path(project.get_path());
    }

    fn on_project_save(&mut self, project: &mut aviutl2::generic::ProjectFile) {
        if let Some(p) = project.get_path() {
            self.set_project_path(Some(p));
        }
    }
}

impl Drop for BackupDiffPlugin {
    fn drop(&mut self) {
        // 本体が DLL を外す前に、履歴のスレッドを止めて終わりを待つ
        worker::shutdown();
    }
}

aviutl2::register_generic_plugin!(BackupDiffPlugin);
