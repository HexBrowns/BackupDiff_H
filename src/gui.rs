//! バックアップ差分のウィンドウ（egui）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use aviutl2::generic::ObjectHandle;
use aviutl2_eframe::{eframe, egui, AviUtl2EframeHandle};
use parking_lot::Mutex;

use crate::aup2::{self, Project};
use crate::backups::{self, Backup, Catalog};
use crate::diff::{self, DiffResult, EffectDiff, ItemChange, Mark, ObjectDiff, Options};
use crate::history::{self, Target};
use crate::inventory::{self, Row};
use crate::dialog;
use crate::live::{self, Restore};
use crate::SharedState;

const GREEN: egui::Color32 = egui::Color32::from_rgb(110, 200, 110);
const RED: egui::Color32 = egui::Color32::from_rgb(235, 110, 110);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(225, 195, 85);

fn color(mark: Mark) -> egui::Color32 {
    match mark {
        Mark::Added => GREEN,
        Mark::Removed => RED,
        Mark::Changed => YELLOW,
    }
}

/// B に選べるもの
#[derive(Clone, PartialEq)]
enum Source {
    Backup(PathBuf),
    /// 保存済みのプロジェクトファイル（最後に保存した内容）
    Saved(PathBuf),
    /// 編集中の表示中のシーン（本体の API で読む）
    Live,
}

struct Comparison {
    label_a: String,
    label_b: String,
    source_b: Source,
    a: Project,
    b: Project,
    /// B が編集中のシーンのとき、`b.objects` と同じ並びのハンドル（「戻す」に使う）
    handles: Option<Vec<ObjectHandle>>,
    only_scene: Option<i32>,
    result: DiffResult,
    notes: Vec<String>,
}

/// 結果の行から起こす操作（描画が終わってからまとめて行う）
enum Action {
    Restore { handle: ObjectHandle, pos: usize, effect_name: String, what: Restore },
    History { anchor_index: usize, target: Target },
}

/// 効果の項目の行が、どのオブジェクトのどの効果のものか（B 側）
#[derive(Clone, Copy)]
struct EffectPlace<'a> {
    b_index: usize,
    pos_b: usize,
    effect_name: &'a str,
}

pub struct BackupDiffApp {
    _handle: AviUtl2EframeHandle,
    shared: SharedState,
    backup_dir: PathBuf,
    catalog: Catalog,
    backups: Vec<Backup>,
    scanned: bool,
    /// 最後に一覧を作ったときのプロジェクト
    last_project: Option<Option<PathBuf>>,
    only_project: bool,
    sel_a: Option<PathBuf>,
    sel_b: Option<Source>,
    filter: String,
    show_ignored: bool,
    comparison: Option<Comparison>,
    history: history::Shared,
    history_open: bool,
    mode: Mode,
    inv: InventoryView,
    status: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Diff,
    Inventory,
}

/// 棚卸しの対象
#[derive(Clone, Copy, PartialEq, Eq)]
enum InvSource {
    /// 保存済みのプロジェクト 1 本
    Project,
    /// 一覧に出ているバックアップ全部（「このプロジェクトだけ」に従う）
    Backups,
    /// フォルダの下の `.aup2` 全部
    Folder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InvColumn {
    Name,
    Kind,
    Projects,
    Objects,
    LastUsed,
}

struct InventoryView {
    source: InvSource,
    folder: Option<PathBuf>,
    state: inventory::Shared,
    filter: String,
    unused_only: bool,
    sort: InvColumn,
    descending: bool,
}

impl Default for InventoryView {
    fn default() -> Self {
        Self {
            source: InvSource::Backups,
            folder: None,
            state: Arc::default(),
            filter: String::new(),
            unused_only: false,
            sort: InvColumn::Projects,
            descending: true,
        }
    }
}

/// フォルダの下の `.aup2` を、更新日時つきで集める（上限 2000 本）
fn collect_aup2(dir: &Path) -> Vec<inventory::Input> {
    const LIMIT: usize = 2000;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("aup2")) {
                let time = dialog::modified_text(&p).unwrap_or_default();
                out.push((p, time));
                if out.len() >= LIMIT {
                    return out;
                }
            }
        }
    }
    out
}

fn kind_label(t: aviutl2::generic::EffectType) -> &'static str {
    use aviutl2::generic::EffectType;
    match t {
        EffectType::Filter => "フィルタ効果",
        EffectType::Input => "メディア入力",
        EffectType::SceneChange => "シーンチェンジ",
        EffectType::Control => "オブジェクト制御",
        EffectType::Output => "メディア出力",
    }
}

impl BackupDiffApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        handle: AviUtl2EframeHandle,
        shared: SharedState,
        backup_dir: PathBuf,
    ) -> Self {
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals = aviutl2_eframe::aviutl2_visuals();
        });
        cc.egui_ctx.set_fonts(aviutl2_eframe::aviutl2_fonts());
        shared.write().egui_ctx = Some(cc.egui_ctx.clone());
        Self {
            _handle: handle,
            shared,
            backup_dir,
            catalog: Catalog::default(),
            backups: Vec::new(),
            scanned: false,
            last_project: None,
            only_project: true,
            sel_a: None,
            sel_b: None,
            filter: String::new(),
            show_ignored: false,
            comparison: None,
            history: Arc::new(Mutex::new(history::State::default())),
            history_open: false,
            mode: Mode::Diff,
            inv: InventoryView::default(),
            status: String::new(),
        }
    }

    fn project_path(&self) -> Option<PathBuf> {
        self.shared.read().project_path.clone()
    }

    /// 一覧に出すバックアップ（新しい順）
    fn visible(&self) -> Vec<&Backup> {
        let project = self.project_path();
        let filter = if self.only_project { project.as_deref() } else { None };
        backups::filter_by_project(&self.backups, filter)
    }

    fn rescan(&mut self) {
        self.scanned = true;
        match self.catalog.scan(&self.backup_dir) {
            Ok(list) => {
                self.backups = list;
                self.status.clear();
            }
            Err(e) => {
                self.backups.clear();
                self.status = format!("{} を読めませんでした: {e}", self.backup_dir.display());
            }
        }
        self.fix_selection();
    }

    /// 一覧に無くなった選択を外し、空なら既定（最新とその 1 つ前）を選ぶ
    fn fix_selection(&mut self) {
        let paths: Vec<PathBuf> = self.visible().iter().map(|b| b.path.clone()).collect();
        if self.sel_a.as_ref().is_some_and(|p| !paths.contains(p)) {
            self.sel_a = None;
        }
        if matches!(&self.sel_b, Some(Source::Backup(p)) if !paths.contains(p)) {
            self.sel_b = None;
        }
        if self.sel_b.is_none() {
            self.sel_b = paths.first().cloned().map(Source::Backup);
        }
        if self.sel_a.is_none() {
            self.sel_a = paths.get(1).or(paths.first()).cloned();
        }
    }

    fn backup_stamp(&self, path: &Path) -> Option<&str> {
        self.backups.iter().find(|b| b.path == path).map(|b| b.stamp.as_str())
    }

    fn backup_label(&self, path: &Path) -> String {
        self.backup_stamp(path).map(str::to_string).unwrap_or_else(|| path.display().to_string())
    }

    fn source_label(&self, s: &Source) -> String {
        match s {
            Source::Backup(p) => self.backup_label(p),
            Source::Saved(_) => "保存済みのプロジェクト".into(),
            Source::Live => "編集中のシーン".into(),
        }
    }

    fn load(path: &Path, label: &str, notes: &mut Vec<String>) -> Result<Project, String> {
        let text = backups::read_shared(path).map_err(|e| format!("{label} を読めませんでした: {e}"))?;
        let p = aup2::parse(&text);
        if p.broken_lines > 0 {
            notes.push(format!("{label}: 読めない行が {} 行ありました（途中で切れている可能性があります）", p.broken_lines));
        }
        Ok(p)
    }

    fn options(&self, only_scene: Option<i32>) -> Options {
        Options { show_ignored: self.show_ignored, only_scene }
    }

    /// 「比較」ボタン。B が編集中のシーンなら、ここで `call_read_section` を 1 回呼ぶ
    fn compare(&mut self) {
        let (Some(a), Some(b)) = (self.sel_a.clone(), self.sel_b.clone()) else {
            self.status = "A と B を選んでください".into();
            return;
        };
        let started = Instant::now();
        let label_a = self.backup_label(&a);
        let label_b = self.source_label(&b);
        let mut notes = Vec::new();
        let pa = match Self::load(&a, &label_a, &mut notes) {
            Ok(p) => p,
            Err(e) => {
                self.status = e;
                self.comparison = None;
                return;
            }
        };
        let (pb, handles, only_scene) = match &b {
            Source::Backup(p) | Source::Saved(p) => match Self::load(p, &label_b, &mut notes) {
                Ok(pb) => (pb, None, None),
                Err(e) => {
                    self.status = e;
                    self.comparison = None;
                    return;
                }
            },
            Source::Live => match live::read_current_scene(&pa) {
                Ok(s) => {
                    notes.push(format!(
                        "編集中のシーン「{}」だけを比べています（他のシーンは本体の API で読めません）",
                        s.scene_name
                    ));
                    if !pa.scenes.iter().any(|x| x.id == s.scene_id) {
                        notes.push("A にこのシーンがありません。全部が追加として出ます".into());
                    }
                    if s.unreadable > 0 {
                        notes.push(format!("読めなかったオブジェクトが {} 個あります", s.unreadable));
                    }
                    (s.project, Some(s.handles), Some(s.scene_id))
                }
                Err(e) => {
                    self.status = e;
                    self.comparison = None;
                    return;
                }
            },
        };
        if matches!(b, Source::Saved(_)) {
            notes.push("B は最後に保存した内容です（保存していない変更は入りません）".into());
        }
        let result = diff::diff(&pa, &pb, self.options(only_scene));
        let (added, removed, changed) = result.object_counts();
        // 値・本文・パスは個人の情報を含むので、ログには件数だけを出す
        tracing::info!(
            "比較: {label_a} → {label_b}（追加 {added} / 削除 {removed} / 変更 {changed}、{:.3} 秒）",
            started.elapsed().as_secs_f64()
        );
        self.status = format!("比較しました（{:.2} 秒）", started.elapsed().as_secs_f64());
        self.comparison = Some(Comparison { label_a, label_b, source_b: b, a: pa, b: pb, handles, only_scene, result, notes });
    }

    fn rediff(&mut self) {
        let opt = self.comparison.as_ref().map(|c| self.options(c.only_scene));
        if let (Some(c), Some(opt)) = (&mut self.comparison, opt) {
            c.result = diff::diff(&c.a, &c.b, opt);
        }
    }

    fn run_actions(&mut self, actions: Vec<Action>, ctx: &egui::Context) {
        for action in actions {
            match action {
                Action::Restore { handle, pos, effect_name, what } => {
                    let place = match &what {
                        Restore::Item { key, .. } => format!("{effect_name}（{} 番目）/ {key}", pos + 1),
                        Restore::Enable { .. } => format!("{effect_name}（{} 番目）/ 有効・無効", pos + 1),
                    };
                    match live::restore(handle, pos, effect_name, what) {
                        Ok(msg) => {
                            tracing::info!("戻す: {place} を戻した");
                            // 戻した結果を出し直す（読み取りだけ）
                            self.compare();
                            self.status = format!("{msg}（Ctrl+Z で取り消せます）");
                        }
                        Err(e) => {
                            tracing::info!("戻す: {place} は戻さなかった（今の値が比べたときと違う、または書けなかった）");
                            self.status = e;
                        }
                    }
                }
                Action::History { anchor_index, target } => self.start_history(anchor_index, target, ctx),
            }
        }
    }

    fn start_history(&mut self, anchor_index: usize, target: Target, ctx: &egui::Context) {
        let Some(c) = &self.comparison else { return };
        // 起点（B）より古いもの・新しいものに分ける。B が保存済み・編集中なら全部が古い側
        let anchor_stamp = match &c.source_b {
            Source::Backup(p) => self.backup_stamp(p).map(str::to_string),
            _ => None,
        };
        let visible: Vec<(PathBuf, String)> = self.visible().iter().map(|b| (b.path.clone(), b.stamp.clone())).collect();
        let (older, mut newer): (Vec<_>, Vec<_>) = match &anchor_stamp {
            Some(s) => {
                let older = visible.iter().filter(|(_, t)| t < s).cloned().collect();
                let newer = visible.iter().filter(|(_, t)| t > s).cloned().collect();
                (older, newer)
            }
            None => (visible, Vec::new()),
        };
        newer.reverse();
        let req = history::Request {
            target,
            anchor: c.b.clone(),
            anchor_index,
            anchor_label: c.label_b.clone(),
            older,
            newer,
        };
        tracing::info!("履歴: {} を {} 本でたどる", req.target.title(), req.older.len() + req.newer.len());
        history::start(req, Arc::clone(&self.history), Some(ctx.clone()));
        self.history_open = true;
    }

    fn render_top(&mut self, ui: &mut egui::Ui) {
        let project = self.project_path();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut self.only_project, "このプロジェクトだけ").changed() {
                self.fix_selection();
            }
            if ui.button("再読み込み").clicked() {
                self.rescan();
            }
            match &project {
                Some(p) => ui.weak(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                None if self.only_project => ui.weak("未保存のプロジェクトなので、全部のバックアップを出しています"),
                None => ui.weak(""),
            };
        });

        let visible: Vec<(PathBuf, String)> = self.visible().iter().map(|b| (b.path.clone(), b.label())).collect();
        ui.horizontal(|ui| {
            ui.label("A:");
            let text_a = self.sel_a.as_deref().map(|p| self.backup_label(p)).unwrap_or_else(|| "（なし）".into());
            egui::ComboBox::from_id_salt("sel_a").selected_text(text_a).width(170.0).show_ui(ui, |ui| {
                for (path, label) in &visible {
                    ui.selectable_value(&mut self.sel_a, Some(path.clone()), label);
                }
            });
            ui.label("B:");
            let text_b = self.sel_b.as_ref().map(|s| self.source_label(s)).unwrap_or_else(|| "（なし）".into());
            egui::ComboBox::from_id_salt("sel_b").selected_text(text_b).width(170.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.sel_b, Some(Source::Live), "編集中のシーン")
                    .on_hover_text("いま表示しているシーンと比べます。値を A に戻せます");
                if let Some(p) = &project {
                    ui.selectable_value(&mut self.sel_b, Some(Source::Saved(p.clone())), "保存済みのプロジェクト");
                }
                ui.separator();
                for (path, label) in &visible {
                    ui.selectable_value(&mut self.sel_b, Some(Source::Backup(path.clone())), label);
                }
            });
            if ui.button("比較").clicked() {
                self.compare();
            }
        });
        if visible.is_empty() {
            ui.weak("バックアップがありません");
        }

        ui.horizontal(|ui| {
            ui.label("絞り込み:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(160.0).hint_text("効果名・項目名・値"));
            if ui
                .checkbox(&mut self.show_ignored, "無視する行も出す")
                .on_hover_text("選択状態（focus）・グループの開閉・カーソル位置・表示位置・[plugin.N] も比べます")
                .changed()
            {
                self.rediff();
            }
        });
    }

    fn render_result(&self, ui: &mut egui::Ui, actions: &mut Vec<Action>) {
        let Some(c) = &self.comparison else {
            ui.weak("A（古い方）と B（新しい方）を選んで「比較」を押してください");
            return;
        };
        let (added, removed, changed) = c.result.object_counts();
        ui.label(format!("{} → {}　オブジェクト: 追加 {added} / 削除 {removed} / 変更 {changed}", c.label_a, c.label_b));
        for n in &c.notes {
            ui.colored_label(YELLOW, n);
        }
        if c.result.is_empty() {
            ui.weak("違いはありません");
            return;
        }
        let needle = self.filter.trim();
        let mut view = View { c, actions };

        if !c.result.project.is_empty() {
            egui::CollapsingHeader::new("プロジェクト").id_salt("project").default_open(true).show(ui, |ui| {
                for item in c.result.project.iter().filter(|i| item_matches(i, needle)) {
                    view.item_row(ui, item, None);
                }
            });
        }
        for scene in &c.result.scenes {
            let objects: Vec<&ObjectDiff> = scene.objects.iter().filter(|o| o.matches(needle)).collect();
            let settings: Vec<&ItemChange> = scene.settings.iter().filter(|i| item_matches(i, needle)).collect();
            if objects.is_empty() && settings.is_empty() && scene.mark == Mark::Changed {
                continue;
            }
            let title = egui::RichText::new(format!("{} {}（{} 件）", scene.mark.symbol(), scene.name, objects.len()))
                .color(color(scene.mark));
            egui::CollapsingHeader::new(title).id_salt(("scene", scene.id)).default_open(true).show(ui, |ui| {
                for item in settings {
                    view.item_row(ui, item, None);
                }
                for (i, o) in objects.iter().enumerate() {
                    view.object_node(ui, (scene.id, i), o);
                }
            });
        }
        for (name, items) in &c.result.others {
            egui::CollapsingHeader::new(format!("[{name}]")).id_salt(("other", name)).show(ui, |ui| {
                for item in items {
                    view.item_row(ui, item, None);
                }
            });
        }
    }

    fn render_history(&mut self, ui: &mut egui::Ui) {
        let s = self.history.lock();
        ui.horizontal(|ui| {
            ui.strong(format!("履歴: {}", s.title));
            if s.running {
                ui.spinner();
                ui.weak(format!("調べています {} / {}", s.done, s.total));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("閉じる").clicked() {
                    self.history_open = false;
                }
            });
        });
        if s.running {
            return;
        }
        egui::ScrollArea::vertical().id_salt("history").auto_shrink([false, false]).show(ui, |ui| {
            if let Some(l) = &s.lost_after {
                ui.weak(format!("{l} からは、このオブジェクトが見つかりません"));
            }
            for p in s.points.iter().rev() {
                let value = p.value.as_deref().unwrap_or("（この項目が無い）");
                let text = format!("{}  {}", p.label, value);
                let resp = ui.add(egui::Label::new(text.as_str()).selectable(true).wrap());
                resp.context_menu(|ui| {
                    if let Some(v) = &p.value {
                        if ui.button("値をコピー").clicked() {
                            ui.ctx().copy_text(v.clone());
                            ui.close();
                        }
                    }
                    if ui.button("この行をコピー").clicked() {
                        ui.ctx().copy_text(text.clone());
                        ui.close();
                    }
                });
            }
            if let Some(l) = &s.lost_before {
                ui.weak(format!("{l} より前には、このオブジェクトが見つかりません"));
            }
            for n in &s.notes {
                ui.colored_label(YELLOW, n);
            }
            ui.weak("新しい順。値が変わった時刻だけを出しています");
        });
    }
}

/// 効果の棚卸し
impl BackupDiffApp {
    fn start_inventory(&mut self, ctx: &egui::Context) {
        let inputs: Vec<inventory::Input> = match self.inv.source {
            InvSource::Project => match self.project_path() {
                Some(p) => {
                    let t = dialog::modified_text(&p).unwrap_or_default();
                    vec![(p, t)]
                }
                None => {
                    self.status = "プロジェクトが未保存です".into();
                    return;
                }
            },
            InvSource::Backups => self.visible().iter().map(|b| (b.path.clone(), b.stamp.clone())).collect(),
            InvSource::Folder => match &self.inv.folder {
                Some(d) => collect_aup2(d),
                None => {
                    self.status = "フォルダを選んでください".into();
                    return;
                }
            },
        };
        if inputs.is_empty() {
            self.status = "数える .aup2 がありません".into();
            return;
        }
        // 本体の効果の一覧は、ボタンを押したときに 1 回だけ取る（起動の途中に呼ぶと例外になることがある。CurveEditor2）
        let installed = live::EDIT_HANDLE.is_ready().then(|| {
            live::EDIT_HANDLE
                .get_effects()
                .into_iter()
                .map(|e| inventory::Installed { name: e.name, kind: kind_label(e.effect_type) })
                .collect::<Vec<_>>()
        });
        self.status.clear();
        inventory::start(inputs, installed, Arc::clone(&self.inv.state), Some(ctx.clone()));
    }

    fn render_inventory_top(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("対象:");
            ui.selectable_value(&mut self.inv.source, InvSource::Project, "保存済みのプロジェクト");
            ui.selectable_value(&mut self.inv.source, InvSource::Backups, "バックアップ")
                .on_hover_text("一覧に出ているバックアップ全部。「このプロジェクトだけ」に従います（差分の画面で切り替え）");
            ui.selectable_value(&mut self.inv.source, InvSource::Folder, "フォルダ");
        });
        ui.horizontal(|ui| {
            match self.inv.source {
                InvSource::Project => {
                    let name = self.project_path().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
                    ui.weak(name.unwrap_or_else(|| "（未保存）".into()));
                }
                InvSource::Backups => {
                    let scope = if self.only_project && self.project_path().is_some() { "このプロジェクトの" } else { "全部の" };
                    ui.weak(format!("{scope}バックアップ {} 本", self.visible().len()));
                }
                InvSource::Folder => {
                    if ui.button("フォルダを選ぶ").clicked() {
                        if let Some(d) = dialog::pick_folder() {
                            self.inv.folder = Some(d);
                        }
                    }
                    let text = self.inv.folder.as_ref().map(|d| d.display().to_string()).unwrap_or_else(|| "（未選択）".into());
                    ui.weak(text).on_hover_text("この下の .aup2 を全部数えます（サブフォルダも）");
                }
            }
            let running = self.inv.state.lock().running;
            if ui.add_enabled(!running, egui::Button::new("数える")).clicked() {
                self.start_inventory(ui.ctx());
            }
        });
        ui.horizontal(|ui| {
            ui.label("絞り込み:");
            ui.add(egui::TextEdit::singleline(&mut self.inv.filter).desired_width(160.0).hint_text("効果名・種類・プロジェクト"));
            ui.checkbox(&mut self.inv.unused_only, "使っていないものだけ");
        });
    }

    fn sorted_rows(&self, rows: &[Row]) -> Vec<Row> {
        let needle = self.inv.filter.trim();
        let mut out: Vec<Row> = rows
            .iter()
            .filter(|r| !self.inv.unused_only || r.projects == 0)
            .filter(|r| needle.is_empty() || r.name.contains(needle) || r.kind.contains(needle) || r.last_project.contains(needle))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            let ord = match self.inv.sort {
                InvColumn::Name => a.name.cmp(&b.name),
                InvColumn::Kind => a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)),
                InvColumn::Projects => a.projects.cmp(&b.projects).then_with(|| a.objects.cmp(&b.objects)),
                InvColumn::Objects => a.objects.cmp(&b.objects).then_with(|| a.projects.cmp(&b.projects)),
                InvColumn::LastUsed => a.last_used.cmp(&b.last_used),
            };
            if self.inv.descending {
                ord.reverse()
            } else {
                ord
            }
        });
        out
    }

    fn render_inventory(&mut self, ui: &mut egui::Ui) {
        let (running, done, total, finished) = {
            let s = self.inv.state.lock();
            (s.running, s.done, s.total, s.finished)
        };
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(format!("数えています {done} / {total}"));
            });
            return;
        }
        if !finished {
            ui.weak("対象を選んで「数える」を押してください。読むだけで、効果を外したり移したりはしません");
            return;
        }
        let (rows, summary, notes, has_installed) = {
            let s = self.inv.state.lock();
            let unused = s.rows.iter().filter(|r| r.projects == 0).count();
            let missing = s.rows.iter().filter(|r| r.kind == inventory::NOT_INSTALLED).count();
            let summary = match s.installed {
                Some(n) => format!(
                    "{} 本 / プロジェクト {} 個。導入済みの効果 {n} 種類のうち、使っていないもの {unused} 種類・使っているが未導入のもの {missing} 種類",
                    s.files, s.projects
                ),
                None => format!("{} 本 / プロジェクト {} 個（本体の効果の一覧を取れなかったので、使っていない効果は出ません）", s.files, s.projects),
            };
            (self.sorted_rows(&s.rows), summary, s.notes.clone(), s.installed.is_some())
        };
        ui.horizontal(|ui| {
            ui.label(summary);
        });
        for n in &notes {
            ui.colored_label(YELLOW, n);
        }
        ui.horizontal(|ui| {
            ui.weak(format!("表示 {} 行", rows.len()));
            if ui.button("CSV をコピー").clicked() {
                ui.ctx().copy_text(inventory::to_csv(&rows));
                self.status = format!("{} 行を CSV でコピーしました", rows.len());
            }
            if ui.button("CSV を保存…").clicked() {
                if let Some(path) = dialog::save_csv("効果の棚卸し.csv") {
                    match std::fs::write(&path, inventory::to_csv(&rows)) {
                        Ok(()) => self.status = format!("{} に保存しました", path.display()),
                        Err(e) => self.status = format!("{} に保存できませんでした: {e}", path.display()),
                    }
                }
            }
        });
        ui.separator();

        const W_KIND: f32 = 96.0;
        const W_NUM: f32 = 72.0;
        const W_TIME: f32 = 132.0;
        let w_name = (ui.available_width() - W_KIND - W_NUM * 2.0 - W_TIME - 160.0).max(160.0);
        ui.horizontal(|ui| {
            for (col, label, width) in [
                (InvColumn::Name, "効果名", w_name),
                (InvColumn::Kind, "種類", W_KIND),
                (InvColumn::Projects, "プロジェクト", W_NUM),
                (InvColumn::Objects, "オブジェクト", W_NUM),
                (InvColumn::LastUsed, "最後に使った日時", W_TIME),
            ] {
                let mark = if self.inv.sort == col { if self.inv.descending { " ▼" } else { " ▲" } } else { "" };
                if ui.add_sized([width, 18.0], egui::Button::new(format!("{label}{mark}")).frame(false)).clicked() {
                    if self.inv.sort == col {
                        self.inv.descending = !self.inv.descending;
                    } else {
                        self.inv.sort = col;
                        self.inv.descending = matches!(col, InvColumn::Projects | InvColumn::Objects | InvColumn::LastUsed);
                    }
                }
            }
            ui.label("最後に使ったプロジェクト");
        });
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_height, rows.len(), |ui, range| {
            for r in &rows[range] {
                ui.horizontal(|ui| {
                    let weak = r.projects == 0;
                    let text = |s: &str| {
                        let t = egui::RichText::new(s);
                        if weak {
                            t.weak()
                        } else {
                            t
                        }
                    };
                    ui.add_sized([w_name, row_height], egui::Label::new(text(&r.name)).truncate())
                        .on_hover_text(&r.name)
                        .context_menu(|ui| {
                            if ui.button("効果名をコピー").clicked() {
                                ui.ctx().copy_text(r.name.clone());
                                ui.close();
                            }
                        });
                    let kind_color = if r.kind == inventory::NOT_INSTALLED { RED } else { ui.visuals().weak_text_color() };
                    ui.add_sized([W_KIND, row_height], egui::Label::new(egui::RichText::new(&r.kind).color(kind_color)).truncate());
                    ui.add_sized([W_NUM, row_height], egui::Label::new(text(&r.projects.to_string())));
                    ui.add_sized([W_NUM, row_height], egui::Label::new(text(&r.objects.to_string())));
                    ui.add_sized([W_TIME, row_height], egui::Label::new(text(r.last_used.as_deref().unwrap_or("—"))));
                    ui.add(egui::Label::new(text(&r.last_project)).truncate());
                });
            }
        });
        if !has_installed {
            ui.weak("本体の効果の一覧を取れませんでした");
        }
    }
}

/// 結果の描画（操作は `actions` に積む）
struct View<'a> {
    c: &'a Comparison,
    actions: &'a mut Vec<Action>,
}

impl View<'_> {
    fn item_row(&mut self, ui: &mut egui::Ui, item: &ItemChange, place: Option<EffectPlace>) {
        let text = if item.key == "effect.disable" {
            format!("有効・無効: {} → {}", on_off(item.old.as_deref()), on_off(item.new.as_deref()))
        } else {
            item.text()
        };
        let mark = item.mark();
        let restore = place.and_then(|p| self.restore_action(item, p));
        let history = place.map(|p| self.history_action(item, p));
        ui.horizontal(|ui| {
            ui.colored_label(color(mark), mark.symbol());
            let resp = ui.add(egui::Label::new(egui::RichText::new(&text).color(color(mark))).selectable(true).wrap());
            let mut chosen: Option<Action> = None;
            resp.context_menu(|ui| {
                if ui.button("この行をコピー").clicked() {
                    ui.ctx().copy_text(text.clone());
                    ui.close();
                }
                for (label, value) in [("古い値をコピー", &item.old), ("新しい値をコピー", &item.new)] {
                    if let Some(v) = value {
                        if ui.button(label).clicked() {
                            ui.ctx().copy_text(v.clone());
                            ui.close();
                        }
                    }
                }
                if let Some(h) = &history {
                    ui.separator();
                    if ui.button("この項目の履歴").clicked() {
                        chosen = Some(h());
                        ui.close();
                    }
                }
            });
            if let Some((label, action)) = restore {
                let hint = "A の値に戻します（本体の Undo 1 回分。Ctrl+Z で取り消せます）";
                if ui.small_button(label).on_hover_text(hint).clicked() {
                    chosen = Some(action());
                }
            }
            if let Some(a) = chosen {
                self.actions.push(a);
            }
        });
    }

    /// 編集中のシーンと比べているときだけ、A の値に戻すボタンを出す
    fn restore_action(&self, item: &ItemChange, p: EffectPlace) -> Option<(&'static str, Box<dyn Fn() -> Action>)> {
        let handle = *self.c.handles.as_ref()?.get(p.b_index)?;
        let (pos, effect_name) = (p.pos_b, p.effect_name.to_string());
        if item.key == "effect.disable" {
            let expected = on_off(item.new.as_deref()) == "有効";
            let value = on_off(item.old.as_deref()) == "有効";
            return Some((
                "戻す",
                Box::new(move || Action::Restore { handle, pos, effect_name: effect_name.clone(), what: Restore::Enable { expected, value } }),
            ));
        }
        let (old, new) = (item.old.clone()?, item.new.clone()?);
        let key = item.key.clone();
        Some((
            "戻す",
            Box::new(move || Action::Restore {
                handle,
                pos,
                effect_name: effect_name.clone(),
                what: Restore::Item { key: key.clone(), expected: new.clone(), value: old.clone() },
            }),
        ))
    }

    fn history_action(&self, item: &ItemChange, p: EffectPlace) -> Box<dyn Fn() -> Action> {
        let names = self.c.b.objects.get(p.b_index).map(|o| o.effect_names()).unwrap_or_default();
        let occurrence = names.iter().take(p.pos_b).filter(|n| **n == p.effect_name).count();
        let target = Target { effect_name: p.effect_name.to_string(), occurrence, key: item.key.clone() };
        let anchor_index = p.b_index;
        Box::new(move || Action::History { anchor_index, target: target.clone() })
    }

    fn effect_node(&mut self, ui: &mut egui::Ui, id: egui::Id, e: &EffectDiff, b_index: Option<usize>) {
        let pos = |p: Option<usize>| p.map(|n| format!("{} 番目", n + 1)).unwrap_or_default();
        match e.mark {
            Mark::Added => marked_row(ui, e.mark, &format!("{}（{}）", e.name, pos(e.pos_b))),
            Mark::Removed => marked_row(ui, e.mark, &format!("{}（{}）", e.name, pos(e.pos_a))),
            Mark::Changed => {
                let moved = if e.reordered { format!("（{} → {}）", pos(e.pos_a), pos(e.pos_b)) } else { String::new() };
                let title = egui::RichText::new(format!("~ {}{moved}", e.name)).color(YELLOW);
                if e.items.is_empty() {
                    ui.label(title);
                    return;
                }
                let place = match (b_index, e.pos_b) {
                    (Some(b_index), Some(pos_b)) => Some(EffectPlace { b_index, pos_b, effect_name: &e.name }),
                    _ => None,
                };
                egui::CollapsingHeader::new(title).id_salt(id).default_open(true).show(ui, |ui| {
                    for item in &e.items {
                        self.item_row(ui, item, place);
                    }
                });
            }
        }
    }

    fn object_node(&mut self, ui: &mut egui::Ui, id: (i32, usize), o: &ObjectDiff) {
        if o.mark != Mark::Changed {
            marked_row(ui, o.mark, &o.heading());
            return;
        }
        let stage = o.stage.map(|s| s.label()).unwrap_or_default();
        // 移動だけで中身の変化が無いものは、開いても空なので 1 行で出す
        if o.props.is_empty() && o.effects.is_empty() {
            let resp = ui.label(egui::RichText::new(format!("~ {}", o.heading())).color(YELLOW));
            resp.on_hover_text(format!("対応付け: {stage}（{}）", stage_hint(o)));
            return;
        }
        let b_index = o.b.as_ref().map(|b| b.index);
        let title = egui::RichText::new(format!("~ {}", o.heading())).color(YELLOW);
        let resp = egui::CollapsingHeader::new(title).id_salt(("object", id)).default_open(true).show(ui, |ui| {
            for item in &o.props {
                self.item_row(ui, item, None);
            }
            for (k, e) in o.effects.iter().enumerate() {
                self.effect_node(ui, egui::Id::new(("effect", id, k)), e, b_index);
            }
        });
        resp.header_response.on_hover_text(format!("対応付け: {stage}（{}）", stage_hint(o)));
    }
}

fn on_off(v: Option<&str>) -> &'static str {
    if v.unwrap_or("0").trim() == "1" {
        "無効"
    } else {
        "有効"
    }
}

fn item_matches(item: &ItemChange, needle: &str) -> bool {
    needle.is_empty()
        || item.key.contains(needle)
        || item.old.as_deref().is_some_and(|v| v.contains(needle))
        || item.new.as_deref().is_some_and(|v| v.contains(needle))
}

/// 記号つきの 1 行（オブジェクト・効果の追加と削除）。右クリックでコピーできる
fn marked_row(ui: &mut egui::Ui, mark: Mark, text: &str) {
    ui.horizontal(|ui| {
        ui.colored_label(color(mark), mark.symbol());
        let resp = ui.add(egui::Label::new(egui::RichText::new(text).color(color(mark))).selectable(true).wrap());
        resp.context_menu(|ui| {
            if ui.button("この行をコピー").clicked() {
                ui.ctx().copy_text(text.to_string());
                ui.close();
            }
        });
    });
}

fn stage_hint(o: &ObjectDiff) -> &'static str {
    match o.stage {
        Some(diff::Stage::Exact) => "レイヤー・フレーム・効果の並びが同じもの",
        Some(diff::Stage::Moved) => "種類と本文・ファイルが同じもの。違う組になっていないか確かめてください",
        Some(diff::Stage::Modified) => "同じレイヤーで重なる同じ種類のもの。違う組になっていないか確かめてください",
        None => "",
    }
}

impl eframe::App for BackupDiffApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 初回と、プロジェクトを開き直したときに一覧を読み直す
        let project = self.project_path();
        if !self.scanned || self.last_project.as_ref() != Some(&project) {
            self.last_project = Some(project);
            self.rescan();
        }
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Diff, "差分");
                ui.selectable_value(&mut self.mode, Mode::Inventory, "効果の棚卸し");
            });
            ui.separator();
            match self.mode {
                Mode::Diff => self.render_top(ui),
                Mode::Inventory => self.render_inventory_top(ui),
            }
        });
        egui::Panel::bottom("bottom").show(ui, |ui| {
            ui.small(if self.status.is_empty() { " " } else { &self.status });
        });
        if self.mode == Mode::Inventory {
            egui::CentralPanel::default().show(ui, |ui| {
                self.render_inventory(ui);
            });
            return;
        }
        if self.history_open {
            egui::Panel::bottom("history").resizable(true).default_size(160.0).min_size(80.0).show(ui, |ui| {
                self.render_history(ui);
            });
        }
        // 中央パネルは最後に追加する
        let mut actions = Vec::new();
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.render_result(ui, &mut actions);
            });
        });
        if !actions.is_empty() {
            let ctx = ui.ctx().clone();
            self.run_actions(actions, &ctx);
            ctx.request_repaint();
        }
    }
}
