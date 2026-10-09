//! `Backup/AutoBackup_*.aup2` の列挙と、どのプロジェクトのものかの判定
//!
//! - 読むだけ。書かない・消さない・並べ替えない（`Backup/` は本体の最後の復旧手段。`au2-safety`）
//! - 日時はファイル名（`AutoBackup_YYYY-MM-DD_HH-MM-SS-mmm.aup2`）から読む
//! - プロジェクトは `[project]` の `file=` で判定する（実物 100 本で確かめた。仕様の「未確認」の 1）。
//!   空（未保存の新規プロジェクト）と `.aup2` で終わらないものは「プロジェクト不明」
//! - 一覧の要約はファイルの更新日時と大きさが変わらなければ読み直さない

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::aup2::{self, Summary};

#[derive(Debug, Clone)]
pub struct Backup {
    pub path: PathBuf,
    /// `YYYY-MM-DD HH:MM:SS`（並べ替えにも使う）
    pub stamp: String,
    pub summary: Option<Summary>,
    /// 読めなかったときの理由
    pub error: Option<String>,
}

impl Backup {
    pub fn project_file(&self) -> Option<&str> {
        let f = self.summary.as_ref()?.file.as_str();
        f.to_ascii_lowercase().ends_with(".aup2").then_some(f)
    }

    /// 一覧の 1 行（`2026-10-05 21:14:02  シーン 3 / 120 個`）
    pub fn label(&self) -> String {
        match (&self.summary, &self.error) {
            (Some(s), _) => format!("{}  シーン {} / {} 個", self.stamp, s.scenes, s.objects),
            (None, Some(e)) => format!("{}  （読めません: {e}）", self.stamp),
            (None, None) => self.stamp.clone(),
        }
    }
}

/// `AutoBackup_2026-10-05_21-14-02-073.aup2` → `2026-10-05 21:14:02`
pub fn stamp_of(file_name: &str) -> Option<String> {
    let body = file_name.strip_prefix("AutoBackup_")?.strip_suffix(".aup2")?;
    let (date, time) = body.split_once('_')?;
    let t: Vec<&str> = time.split('-').collect();
    let ok = date.len() == 10
        && t.len() >= 3
        && date.bytes().filter(|b| b.is_ascii_digit()).count() == 8
        && t[..3].iter().all(|x| x.len() == 2 && x.bytes().all(|b| b.is_ascii_digit()));
    ok.then(|| format!("{date} {}:{}:{}", t[0], t[1], t[2]))
}

/// パスを比べるための正規化（区切りを `\` に、大文字小文字を無視）
pub fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.replace('/', "\\").to_lowercase();
    norm(a) == norm(&b.to_string_lossy())
}

/// 共有読み取りで開いて読む（本体が書いている最中のファイルの書き込みを邪魔しない）
pub fn read_shared(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// 要約のキャッシュ（パス → (更新日時, 大きさ, 要約)）
#[derive(Default)]
pub struct Catalog {
    cache: HashMap<PathBuf, (SystemTime, u64, Result<Summary, String>)>,
}

impl Catalog {
    /// `dir` の `AutoBackup_*.aup2` を新しい順に返す
    pub fn scan(&mut self, dir: &Path) -> std::io::Result<Vec<Backup>> {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stamp) = stamp_of(&name) else { continue };
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            let (mtime, len) = (meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len());
            let fresh = matches!(self.cache.get(&path), Some((t, l, _)) if *t == mtime && *l == len);
            if !fresh {
                let summary = read_shared(&path).map(|t| aup2::summarize(&t)).map_err(|e| e.to_string());
                self.cache.insert(path.clone(), (mtime, len, summary));
            }
            let (summary, error) = match &self.cache[&path].2 {
                Ok(s) => (Some(s.clone()), None),
                Err(e) => (None, Some(e.clone())),
            };
            seen.push(path.clone());
            out.push(Backup { path, stamp, summary, error });
        }
        // 消えたファイルの要約は捨てる（本体が古いものから消していく）
        self.cache.retain(|p, _| seen.contains(p));
        out.sort_by(|a, b| b.stamp.cmp(&a.stamp).then_with(|| b.path.cmp(&a.path)));
        Ok(out)
    }
}

/// 「このプロジェクトだけ」の絞り込み。`project` が `None`（未保存）なら絞らない
pub fn filter_by_project<'a>(list: &'a [Backup], project: Option<&Path>) -> Vec<&'a Backup> {
    match project {
        None => list.iter().collect(),
        Some(p) => list.iter().filter(|b| b.project_file().is_some_and(|f| same_path(f, p))).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_from_file_name() {
        assert_eq!(stamp_of("AutoBackup_2026-10-05_21-14-02-073.aup2").as_deref(), Some("2026-10-05 21:14:02"));
        assert_eq!(stamp_of("AutoBackup_x.aup2"), None);
        assert_eq!(stamp_of("Other_2026-10-05_21-14-02-073.aup2"), None);
        assert_eq!(stamp_of("AutoBackup_2026-10-05_21-14-02-073.aup2.tmp"), None);
    }

    #[test]
    fn project_filter_ignores_unknown_and_case() {
        let mk = |file: &str| Backup {
            path: PathBuf::new(),
            stamp: String::new(),
            summary: Some(Summary { file: file.into(), scenes: 1, objects: 0 }),
            error: None,
        };
        let list = vec![mk("D:\\動画\\作品.aup2"), mk(""), mk("D:\\動画\\"), mk("d:/動画/作品.AUP2"), mk("D:\\動画\\別.aup2")];
        assert_eq!(filter_by_project(&list, Some(Path::new("D:\\動画\\作品.aup2"))).len(), 2);
        assert_eq!(filter_by_project(&list, None).len(), 5);
        assert_eq!(list[2].project_file(), None);
    }

    /// 手元の `Backup/` で、一覧・全体パース・比較の時間を測る（無ければ何もしない）。
    /// `cargo test --release -- --ignored --nocapture real_backups` で走らせる
    #[test]
    #[ignore]
    fn real_backups() {
        let dir = Path::new("C:/ProgramData/aviutl2/Backup");
        if !dir.exists() {
            return;
        }
        let t = std::time::Instant::now();
        let mut cat = Catalog::default();
        let list = cat.scan(dir).unwrap();
        let first = t.elapsed();
        let t = std::time::Instant::now();
        let _ = cat.scan(dir).unwrap();
        let second = t.elapsed();
        println!("一覧 {} 本: 初回 {first:?} / 2 回目 {second:?}", list.len());
        assert!(list.iter().all(|b| b.summary.is_some()));

        let biggest = list.iter().max_by_key(|b| std::fs::metadata(&b.path).map(|m| m.len()).unwrap_or(0)).unwrap();
        let same: Vec<&Backup> = list.iter().filter(|b| b.project_file() == biggest.project_file()).collect();
        let pos = same.iter().position(|b| b.path == biggest.path).unwrap();
        let older = same.get(pos + 1).copied().unwrap_or(biggest);
        let t = std::time::Instant::now();
        let a = crate::aup2::parse(&read_shared(&older.path).unwrap());
        let b = crate::aup2::parse(&read_shared(&biggest.path).unwrap());
        let parsed = t.elapsed();
        let t = std::time::Instant::now();
        let r = crate::diff::diff(&a, &b, Default::default());
        let diffed = t.elapsed();
        println!(
            "{} → {}: オブジェクト {} / {} 個、壊れた行 {} / {}、パース 2 本 {parsed:?}、比較 {diffed:?}、結果 {:?}",
            older.stamp, biggest.stamp, a.objects.len(), b.objects.len(), a.broken_lines, b.broken_lines, r.object_counts()
        );
        assert_eq!(a.broken_lines + b.broken_lines, 0);
    }

    /// 環境変数 `BD_A` / `BD_B` の 2 本を比べて結果を出す（目で確かめる用）
    #[test]
    #[ignore]
    fn print_diff() {
        let (Ok(pa), Ok(pb)) = (std::env::var("BD_A"), std::env::var("BD_B")) else {
            return;
        };
        let a = crate::aup2::parse(&read_shared(Path::new(&pa)).unwrap());
        let b = crate::aup2::parse(&read_shared(Path::new(&pb)).unwrap());
        let r = crate::diff::diff(&a, &b, Default::default());
        for c in &r.project {
            println!("[project] {}", c.text());
        }
        for s in &r.scenes {
            println!("{} {}", s.mark.symbol(), s.name);
            for c in &s.settings {
                println!("  {} {}", c.mark().symbol(), c.text());
            }
            for o in &s.objects {
                println!("  {} {} [{}]", o.mark.symbol(), o.heading(), o.stage.map(|x| x.label()).unwrap_or(""));
                for c in &o.props {
                    println!("      {} {}", c.mark().symbol(), c.text());
                }
                for e in &o.effects {
                    println!("    {} {} {:?}->{:?}{}", e.mark.symbol(), e.name, e.pos_a, e.pos_b, if e.reordered { " 並べ替え" } else { "" });
                    for c in &e.items {
                        println!("      {} {}", c.mark().symbol(), c.text());
                    }
                }
            }
        }
    }
}
