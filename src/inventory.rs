//! 効果の棚卸し（第 3 段階の機能 5）
//!
//! `.aup2` を読み、効果名（`effect.name=`）ごとに、使ったプロジェクトの数・オブジェクトの数・最後に使った日時を数える。
//! 本体が列挙する効果（`EDIT_HANDLE.get_effects()`）と突き合わせて、一度も使っていない効果と、もう導入されていない効果も出す。
//!
//! - **同じプロジェクトのバックアップは 1 つのプロジェクトとして数える**（`[project]` の `file=` でまとめる）。
//!   「使ったプロジェクト」はどれかの版に一度でも出てきたもの、オブジェクトの数はそのプロジェクトの最新の版で数える
//! - 読むだけ。効果を外す・移す操作は持たない（第三者の配布物を動かすのは `au2-safety` の範囲）

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aviutl2_eframe::egui;
use parking_lot::Mutex;

use crate::aup2::{self, Project};
use crate::{backups, worker};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// 本体が列挙した効果（名前と種類）
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub kind: &'static str,
}

pub const NOT_INSTALLED: &str = "未導入";

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub name: String,
    /// 本体の効果の種類。本体の一覧に無ければ「未導入」。一覧を取れなかったときは空
    pub kind: String,
    pub projects: usize,
    pub objects: usize,
    /// 最後に使った日時（`YYYY-MM-DD HH:MM:SS`）
    pub last_used: Option<String>,
    pub last_project: String,
}

/// プロジェクトごとの途中経過
#[derive(Default)]
struct ProjectAcc {
    name: String,
    /// 最新の版の (日時, 効果名 → オブジェクト数)
    newest: Option<(String, HashMap<String, usize>)>,
    /// どれかの版に出てきた効果と、出てきた最後の日時
    seen: HashMap<String, String>,
}

/// 数えた結果を貯める
#[derive(Default)]
pub struct Tally {
    projects: BTreeMap<String, ProjectAcc>,
}

/// 効果名ごとの、その版のオブジェクト数（同じオブジェクトに 2 回掛けても 1 と数える）
fn objects_per_effect(p: &Project) -> HashMap<String, usize> {
    let mut out: HashMap<String, usize> = HashMap::new();
    for o in &p.objects {
        let names: HashSet<&str> = o.effects.iter().map(|e| e.name.as_str()).filter(|n| !n.is_empty()).collect();
        for n in names {
            *out.entry(n.to_string()).or_default() += 1;
        }
    }
    out
}

/// まとめる鍵（`file=` が `.aup2` ならそれ、無ければそのファイル自身）と表示名
pub fn project_key(p: &Project, path: &std::path::Path) -> (String, String) {
    let file = aup2::lookup(&p.header, "file").unwrap_or("");
    let key_path = if file.to_ascii_lowercase().ends_with(".aup2") { file.to_string() } else { path.to_string_lossy().into_owned() };
    let base = key_path.rsplit(['\\', '/']).next().unwrap_or(&key_path);
    let name = match base.len().checked_sub(5) {
        Some(n) if base.is_char_boundary(n) && base[n..].eq_ignore_ascii_case(".aup2") => &base[..n],
        _ => base,
    }
    .to_string();
    (key_path.replace('/', "\\").to_lowercase(), name)
}

impl Tally {
    /// 1 本足す。`time` は並べ替えられる日時の文字列
    pub fn add(&mut self, key: String, name: String, time: String, p: &Project) {
        let counts = objects_per_effect(p);
        let acc = self.projects.entry(key).or_default();
        for n in counts.keys() {
            let t = acc.seen.entry(n.clone()).or_insert_with(|| time.clone());
            if *t < time {
                *t = time.clone();
            }
        }
        if acc.newest.as_ref().is_none_or(|(t, _)| *t < time) {
            acc.name = name;
            acc.newest = Some((time, counts));
        }
    }

    pub fn project_count(&self) -> usize {
        self.projects.len()
    }

    /// 表にする。`installed` が `Some` なら、使っていない効果と未導入の効果も出す
    pub fn rows(&self, installed: Option<&[Installed]>) -> Vec<Row> {
        let mut rows: BTreeMap<String, Row> = BTreeMap::new();
        for acc in self.projects.values() {
            for (name, time) in &acc.seen {
                let row = rows.entry(name.clone()).or_insert_with(|| Row {
                    name: name.clone(),
                    kind: String::new(),
                    projects: 0,
                    objects: 0,
                    last_used: None,
                    last_project: String::new(),
                });
                row.projects += 1;
                row.objects += acc.newest.as_ref().and_then(|(_, c)| c.get(name)).copied().unwrap_or(0);
                if row.last_used.as_ref().is_none_or(|t| t < time) {
                    row.last_used = Some(time.clone());
                    row.last_project = acc.name.clone();
                }
            }
        }
        if let Some(installed) = installed {
            let kinds: HashMap<&str, &str> = installed.iter().map(|i| (i.name.as_str(), i.kind)).collect();
            for row in rows.values_mut() {
                row.kind = kinds.get(row.name.as_str()).copied().unwrap_or(NOT_INSTALLED).to_string();
            }
            for i in installed {
                rows.entry(i.name.clone()).or_insert_with(|| Row {
                    name: i.name.clone(),
                    kind: i.kind.to_string(),
                    projects: 0,
                    objects: 0,
                    last_used: None,
                    last_project: String::new(),
                });
            }
        }
        rows.into_values().collect()
    }
}

/// CSV（Excel で開けるように BOM 付き UTF-8・CRLF）
pub fn to_csv(rows: &[Row]) -> String {
    fn field(s: &str) -> String {
        if s.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let mut out = String::from("\u{feff}効果名,種類,プロジェクト数,オブジェクト数,最後に使った日時,最後に使ったプロジェクト\r\n");
    for r in rows {
        out += &format!(
            "{},{},{},{},{},{}\r\n",
            field(&r.name),
            field(&r.kind),
            r.projects,
            r.objects,
            field(r.last_used.as_deref().unwrap_or("")),
            field(&r.last_project)
        );
    }
    out
}

/// 数える対象の 1 本（パスと、並べ替えられる日時）
pub type Input = (PathBuf, String);

#[derive(Default)]
pub struct State {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub rows: Vec<Row>,
    pub projects: usize,
    pub files: usize,
    /// 本体の効果の一覧を取れたか
    pub installed: Option<usize>,
    pub notes: Vec<String>,
    /// 一度でも数え終わったか
    pub finished: bool,
}

pub type Shared = Arc<Mutex<State>>;

/// 裏のスレッドで数える。結果は `shared` に入る
pub fn start(inputs: Vec<Input>, installed: Option<Vec<Installed>>, shared: Shared, ctx: Option<egui::Context>) {
    let gen = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    *shared.lock() = State { running: true, total: inputs.len(), ..Default::default() };
    worker::spawn(move || {
        let mut tally = Tally::default();
        let mut notes = Vec::new();
        let mut files = 0;
        for (path, time) in &inputs {
            if worker::stopping() || GENERATION.load(Ordering::Acquire) != gen {
                return;
            }
            match backups::read_shared(path) {
                Ok(text) => {
                    let p = aup2::parse(&text);
                    let (key, name) = project_key(&p, path);
                    tally.add(key, name, time.clone(), &p);
                    files += 1;
                }
                Err(e) => notes.push(format!("{} を読めませんでした: {e}", path.display())),
            }
            shared.lock().done += 1;
            if let Some(ctx) = &ctx {
                ctx.request_repaint();
            }
        }
        let rows = tally.rows(installed.as_deref());
        let mut s = shared.lock();
        if GENERATION.load(Ordering::Acquire) != gen {
            return;
        }
        tracing::info!(
            "棚卸し: {} 本 / プロジェクト {} 個 / 効果 {} 種類（本体の一覧 {}）",
            files,
            tally.project_count(),
            rows.len(),
            installed.as_ref().map(|i| i.len().to_string()).unwrap_or_else(|| "なし".into())
        );
        *s = State {
            running: false,
            done: inputs.len(),
            total: inputs.len(),
            rows,
            projects: tally.project_count(),
            files,
            installed: installed.as_ref().map(Vec::len),
            notes,
            finished: true,
        };
        drop(s);
        if let Some(ctx) = &ctx {
            ctx.request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn proj(file: &str, objects: &[&[&str]]) -> Project {
        let mut s = format!("[project]\nfile={file}\n[scene.0]\nscene=0\n");
        for (n, effects) in objects.iter().enumerate() {
            s += &format!("[{n}]\nlayer={n}\nframe=0,9\n");
            for (m, e) in effects.iter().enumerate() {
                s += &format!("[{n}.{m}]\neffect.name={e}\n");
            }
        }
        aup2::parse(&s)
    }

    fn add(t: &mut Tally, file: &str, time: &str, objects: &[&[&str]]) {
        let p = proj(file, objects);
        let (k, n) = project_key(&p, Path::new("C:\\Backup\\x.aup2"));
        t.add(k, n, time.into(), &p);
    }

    #[test]
    fn backups_of_one_project_count_once() {
        let mut t = Tally::default();
        // 同じプロジェクトの 2 版。古い版にだけ「ぼかし」、新しい版では図形が 2 個
        add(&mut t, "D:\\作品\\A.aup2", "2026-10-01 10:00:00", &[&["図形", "ぼかし", "ぼかし", "標準描画"]]);
        add(&mut t, "d:/作品/a.AUP2", "2026-10-02 10:00:00", &[&["図形", "標準描画"], &["図形", "標準描画"]]);
        add(&mut t, "D:\\作品\\B.aup2", "2026-09-01 10:00:00", &[&["テキスト", "ぼかし", "標準描画"]]);
        assert_eq!(t.project_count(), 2);
        let rows = t.rows(None);
        let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap().clone();
        let fig = get("図形");
        assert_eq!((fig.projects, fig.objects, fig.last_used.as_deref()), (1, 2, Some("2026-10-02 10:00:00")));
        let blur = get("ぼかし");
        // A では古い版にだけある（最新の版では 0 個）。B の最新の版で 1 個
        assert_eq!((blur.projects, blur.objects), (2, 1));
        // プロジェクト名は最新の版の file= から取る（大文字小文字の違う 2 本は同じプロジェクト）
        assert_eq!((blur.last_used.as_deref(), blur.last_project.as_str()), (Some("2026-10-01 10:00:00"), "a"));
    }

    #[test]
    fn installed_list_adds_unused_and_marks_missing() {
        let mut t = Tally::default();
        add(&mut t, "D:\\A.aup2", "2026-10-01 10:00:00", &[&["図形", "古いスクリプト@消えた"]]);
        let installed = vec![
            Installed { name: "図形".into(), kind: "メディア入力" },
            Installed { name: "グロー".into(), kind: "フィルタ効果" },
        ];
        let rows = t.rows(Some(&installed));
        let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap().clone();
        assert_eq!((get("図形").kind.as_str(), get("図形").projects), ("メディア入力", 1));
        assert_eq!((get("グロー").kind.as_str(), get("グロー").projects, get("グロー").last_used.clone()), ("フィルタ効果", 0, None));
        assert_eq!(get("古いスクリプト@消えた").kind, NOT_INSTALLED);
    }

    #[test]
    fn unsaved_backup_is_its_own_project() {
        let p = proj("", &[&["図形"]]);
        let (k, n) = project_key(&p, Path::new("C:\\Backup\\AutoBackup_1.aup2"));
        assert_eq!((k.as_str(), n.as_str()), ("c:\\backup\\autobackup_1.aup2", "AutoBackup_1"));
    }

    /// 手元の `Backup/` 全部と `D:/Videos/動画編集` で数える時間を測る（本体の一覧は無し）。
    /// `cargo test --release -- --ignored --nocapture real_inventory`
    #[test]
    #[ignore]
    fn real_inventory() {
        let mut sets: Vec<(&str, Vec<Input>)> = Vec::new();
        let dir = std::path::Path::new("C:/ProgramData/aviutl2/Backup");
        if dir.exists() {
            let list = backups::Catalog::default().scan(dir).unwrap();
            sets.push(("Backup", list.into_iter().map(|b| (b.path, b.stamp)).collect()));
        }
        let videos = std::path::Path::new("D:/Videos/動画編集");
        if videos.exists() {
            let mut files = Vec::new();
            let mut stack = vec![videos.to_path_buf()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("aup2")) {
                        files.push((p, String::new()));
                    }
                }
            }
            sets.push(("動画編集", files));
        }
        for (label, inputs) in sets {
            let n = inputs.len();
            let shared: Shared = Arc::default();
            let t = std::time::Instant::now();
            start(inputs, None, Arc::clone(&shared), None);
            while shared.lock().running {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let s = shared.lock();
            let mut top = s.rows.clone();
            top.sort_by(|a, b| b.projects.cmp(&a.projects).then(b.objects.cmp(&a.objects)));
            let top: Vec<String> = top.iter().take(8).map(|r| format!("{}({}/{})", r.name, r.projects, r.objects)).collect();
            println!("{label}: {n} 本 → プロジェクト {} 個・効果 {} 種類、{:?}。上位 {:?}", s.projects, s.rows.len(), t.elapsed(), top);
            assert!(s.finished);
        }
    }

    #[test]
    fn csv_quotes_and_has_bom() {
        let rows = vec![Row {
            name: "a,b\"c".into(),
            kind: "フィルタ効果".into(),
            projects: 1,
            objects: 2,
            last_used: Some("2026-10-01 10:00:00".into()),
            last_project: "作品".into(),
        }];
        let csv = to_csv(&rows);
        assert!(csv.starts_with('\u{feff}'));
        assert!(csv.ends_with("\"a,b\"\"c\",フィルタ効果,1,2,2026-10-01 10:00:00,作品\r\n"), "{csv}");
    }
}
