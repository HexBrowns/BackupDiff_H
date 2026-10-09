//! 1 項目の移り変わり（第 2 段階の機能 3）
//!
//! 結果の項目を起点に、同じオブジェクトをバックアップの間で 1 本ずつ対応付けてたどり（`diff::counterpart`）、
//! その項目の値を時系列に並べる。値が変わった時刻だけを残す。
//! 100 本で 0.5 秒前後かかるので、裏のスレッドで行う（`worker`）。新しい依頼が来たら古いものは途中でやめる

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use aviutl2_eframe::egui;
use parking_lot::Mutex;

use crate::aup2::{self, Object, Project};
use crate::{backups, diff, worker};

static GENERATION: AtomicU64 = AtomicU64::new(0);

/// どの項目をたどるか
#[derive(Debug, Clone)]
pub struct Target {
    pub effect_name: String,
    /// 同名の効果の何番目か（0 始まり）
    pub occurrence: usize,
    pub key: String,
}

impl Target {
    pub fn value_in(&self, o: &Object) -> Option<String> {
        let effect = o.effects.iter().filter(|e| e.name == self.effect_name).nth(self.occurrence)?;
        match aup2::lookup(&effect.entries, &self.key) {
            Some(v) => Some(v.to_string()),
            // effect.disable が無いのは有効（=0）
            None if self.key == "effect.disable" => Some("0".into()),
            None => None,
        }
    }

    pub fn title(&self) -> String {
        if self.occurrence == 0 {
            format!("{} / {}", self.effect_name, self.key)
        } else {
            format!("{}（{} 個目）/ {}", self.effect_name, self.occurrence + 1, self.key)
        }
    }
}

pub struct Request {
    pub target: Target,
    /// 起点（比較の B）
    pub anchor: Project,
    pub anchor_index: usize,
    pub anchor_label: String,
    /// 起点より古いバックアップ（新しい順）
    pub older: Vec<(PathBuf, String)>,
    /// 起点より新しいバックアップ（古い順）
    pub newer: Vec<(PathBuf, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    pub label: String,
    /// `None` は、オブジェクトはあるがその項目（効果）が無い
    pub value: Option<String>,
}

#[derive(Default)]
pub struct State {
    pub title: String,
    pub running: bool,
    pub done: usize,
    pub total: usize,
    /// 古い順。値が変わった所だけ
    pub points: Vec<Point>,
    /// たどれなくなった所（そこより古い / 新しいバックアップには、組になるオブジェクトが無い）
    pub lost_before: Option<String>,
    pub lost_after: Option<String>,
    pub notes: Vec<String>,
}

pub type Shared = Arc<Mutex<State>>;

/// 値が同じ点が続いたら、最初の 1 つだけを残す
pub fn compress(points: Vec<Point>) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    for p in points {
        if out.last().is_some_and(|last| last.value == p.value) {
            continue;
        }
        out.push(p);
    }
    out
}

/// 起点から 1 方向へたどる。`(点の列, たどれなくなったバックアップ)`
fn walk(
    req: &Request,
    list: &[(PathBuf, String)],
    gen: u64,
    shared: &Shared,
    ctx: &Option<egui::Context>,
) -> Option<(Vec<Point>, Option<String>, Vec<String>)> {
    let mut points = Vec::new();
    let mut notes = Vec::new();
    let mut current = req.anchor.clone();
    let mut index = req.anchor_index;
    for (path, label) in list {
        if worker::stopping() || GENERATION.load(Ordering::Acquire) != gen {
            return None;
        }
        let next = match backups::read_shared(path) {
            Ok(t) => aup2::parse(&t),
            Err(e) => {
                notes.push(format!("{label} を読めませんでした: {e}"));
                shared.lock().done += 1;
                continue;
            }
        };
        match diff::counterpart(&current, index, &next) {
            Some(j) => {
                points.push(Point { label: label.clone(), value: req.target.value_in(&next.objects[j]) });
                current = next;
                index = j;
            }
            None => return Some((points, Some(label.clone()), notes)),
        }
        shared.lock().done += 1;
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    }
    Some((points, None, notes))
}

/// 裏のスレッドで履歴を作る。結果は `shared` に入る
pub fn start(req: Request, shared: Shared, ctx: Option<egui::Context>) {
    let gen = GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    {
        let mut s = shared.lock();
        *s = State {
            title: req.target.title(),
            running: true,
            total: req.older.len() + req.newer.len(),
            ..Default::default()
        };
    }
    worker::spawn(move || {
        let older = walk(&req, &req.older, gen, &shared, &ctx);
        let newer = walk(&req, &req.newer, gen, &shared, &ctx);
        let (Some((older, lost_before, mut notes)), Some((newer, lost_after, notes_after))) = (older, newer) else {
            return; // 止めた・新しい依頼が来た
        };
        notes.extend(notes_after);
        let anchor_value = req.target.value_in(&req.anchor.objects[req.anchor_index]);
        let mut points: Vec<Point> = older.into_iter().rev().collect();
        points.push(Point { label: req.anchor_label.clone(), value: anchor_value });
        points.extend(newer);
        let mut s = shared.lock();
        if GENERATION.load(Ordering::Acquire) != gen {
            return;
        }
        s.points = compress(points);
        tracing::info!("履歴: {} の値の変わり目 {} 個（見失った所: 前 {} / 後 {}）",
            req.target.title(), s.points.len(), lost_before.is_some(), lost_after.is_some());
        s.lost_before = lost_before;
        s.lost_after = lost_after;
        s.notes = notes;
        s.running = false;
        drop(s);
        if let Some(ctx) = &ctx {
            ctx.request_repaint();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(label: &str, v: Option<&str>) -> Point {
        Point { label: label.into(), value: v.map(str::to_string) }
    }

    /// 手元の `Backup/` で、最新の 1 本の `標準描画 / X` を全部のバックアップでたどる時間を測る。
    /// `cargo test --release -- --ignored --nocapture real_history`
    #[test]
    #[ignore]
    fn real_history() {
        let dir = std::path::Path::new("C:/ProgramData/aviutl2/Backup");
        if !dir.exists() {
            return;
        }
        let list = backups::Catalog::default().scan(dir).unwrap();
        // バックアップの本数が一番多いプロジェクトの、最新の 1 本を起点にする
        let count = |f: Option<&str>| list.iter().filter(|b| b.project_file() == f).count();
        let newest = list.iter().max_by_key(|b| (count(b.project_file()), std::cmp::Reverse(0))).unwrap();
        let same: Vec<_> = list.iter().filter(|b| b.project_file() == newest.project_file()).collect();
        let newest = same[0];
        let anchor = aup2::parse(&backups::read_shared(&newest.path).unwrap());
        let index = anchor.objects.iter().position(|o| o.effects.iter().any(|e| e.name == "標準描画")).unwrap();
        let req = Request {
            target: Target { effect_name: "標準描画".into(), occurrence: 0, key: "X".into() },
            anchor,
            anchor_index: index,
            anchor_label: newest.stamp.clone(),
            older: same[1..].iter().map(|b| (b.path.clone(), b.stamp.clone())).collect(),
            newer: Vec::new(),
        };
        let shared: Shared = Arc::default();
        let t = std::time::Instant::now();
        start(req, Arc::clone(&shared), None);
        while shared.lock().running {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let s = shared.lock();
        println!(
            "{} 本をたどって {:?}: 点 {} / 見失った {:?} / {:?}",
            same.len() - 1,
            t.elapsed(),
            s.points.len(),
            s.lost_before,
            s.points.iter().map(|p| (p.label.as_str(), p.value.as_deref())).collect::<Vec<_>>()
        );
        assert!(!s.points.is_empty());
    }

    #[test]
    fn walks_backups_and_keeps_changes() {
        // 古い順に 4 本。対象（図形の X）は 2 本目で前にオブジェクトが挿入されて番号がずれ、3 本目で値が変わる
        let obj = |n: usize, layer: usize, start: usize, x: &str| {
            format!("[{n}]\nlayer={layer}\nframe={start},{}\n[{n}.0]\neffect.name=図形\n[{n}.1]\neffect.name=標準描画\nX={x}\n", start + 9)
        };
        let head = "[project]\nfile=C:\\p.aup2\n[scene.0]\nscene=0\nname=Root\n";
        let files = [
            format!("{head}{}", obj(0, 1, 0, "10")),
            format!("{head}{}{}", obj(0, 0, 0, "99"), obj(1, 1, 0, "10")),
            format!("{head}{}{}", obj(0, 0, 0, "99"), obj(1, 1, 0, "20")),
            format!("{head}{}{}", obj(0, 0, 0, "99"), obj(1, 1, 0, "20")),
        ];
        let dir = std::env::temp_dir().join(format!("backup_diff_h_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<(PathBuf, String)> = files
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let p = dir.join(format!("{i}.aup2"));
                std::fs::write(&p, t).unwrap();
                (p, format!("t{i}"))
            })
            .collect();
        let anchor = aup2::parse(&files[3]);
        let req = Request {
            target: Target { effect_name: "標準描画".into(), occurrence: 0, key: "X".into() },
            anchor_index: 1,
            anchor,
            anchor_label: "t3".into(),
            older: paths[..3].iter().rev().cloned().collect(),
            newer: Vec::new(),
        };
        let shared: Shared = Arc::default();
        start(req, Arc::clone(&shared), None);
        while shared.lock().running {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let s = shared.lock();
        assert_eq!(s.points, [p("t0", Some("10")), p("t2", Some("20"))]);
        assert_eq!(s.lost_before, None);
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn compress_keeps_first_of_each_run() {
        let pts = vec![p("1", Some("a")), p("2", Some("a")), p("3", Some("b")), p("4", None), p("5", None), p("6", Some("a"))];
        assert_eq!(compress(pts), [p("1", Some("a")), p("3", Some("b")), p("4", None), p("6", Some("a"))]);
    }

    #[test]
    fn target_reads_nth_same_named_effect() {
        let text = "[0]\nlayer=0\nframe=0,9\n[0.0]\neffect.name=図形\n[0.1]\neffect.name=ぼかし\n範囲=5\n[0.2]\neffect.name=ぼかし\n範囲=9\neffect.disable=1\n";
        let o = &aup2::parse(text).objects[0];
        let t = |occurrence, key: &str| Target { effect_name: "ぼかし".into(), occurrence, key: key.into() };
        assert_eq!(t(0, "範囲").value_in(o).as_deref(), Some("5"));
        assert_eq!(t(1, "範囲").value_in(o).as_deref(), Some("9"));
        assert_eq!(t(0, "effect.disable").value_in(o).as_deref(), Some("0"));
        assert_eq!(t(1, "effect.disable").value_in(o).as_deref(), Some("1"));
        assert_eq!(t(2, "範囲").value_in(o), None);
        assert_eq!(t(1, "範囲").title(), "ぼかし（2 個目）/ 範囲");
    }
}
