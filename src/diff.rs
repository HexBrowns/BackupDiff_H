//! 2 つの `.aup2` の対応付けと比較
//!
//! オブジェクト番号 `[N]` は (シーン, レイヤー, 開始フレーム) の順で振り直されるので、番号では組にしない。
//! シーンごとに次の段で組にする（仕様 `AI/specifications/20261005_BackupDiff_H_spec.md` の「オブジェクトの対応付け」）。
//!
//! 1. 完全一致: レイヤー・`frame=`・効果名の列が同じ
//! 2. 移動: 先頭の効果名と識別値（`テキスト=` / `ファイル=`、無ければ効果名の列）が同じ。
//!    候補が複数なら、フレームの重なりが大きい順、次にレイヤーの差が小さい順
//! 3. 変更: 同じレイヤーで、フレームの範囲が重なり、先頭の効果名が同じ
//! 4. 残りは追加・削除
//!
//! 効果は効果名の列の最長共通部分列で組にし、残った同名の効果は「並べ替え」として組にする。

use std::collections::{HashMap, VecDeque};

use crate::aup2::{Effect, Entries, Object, Project};

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// 既定で比べない行（UI の状態など）も比べる
    pub show_ignored: bool,
    /// このシーンのオブジェクトだけを比べる（プロジェクトとシーンの設定は比べない）。編集中のシーンと比べるとき
    pub only_scene: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Added,
    Removed,
    Changed,
}

impl Mark {
    pub fn symbol(self) -> &'static str {
        match self {
            Mark::Added => "+",
            Mark::Removed => "-",
            Mark::Changed => "~",
        }
    }
}

/// オブジェクトをどの段で組にしたか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Exact,
    Moved,
    Modified,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Exact => "完全一致",
            Stage::Moved => "移動",
            Stage::Modified => "変更",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemChange {
    pub key: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

impl ItemChange {
    pub fn mark(&self) -> Mark {
        match (&self.old, &self.new) {
            (None, Some(_)) => Mark::Added,
            (Some(_), None) => Mark::Removed,
            _ => Mark::Changed,
        }
    }

    /// `項目名: 旧 → 新`
    pub fn text(&self) -> String {
        match (&self.old, &self.new) {
            (Some(o), Some(n)) => format!("{}: {} → {}", self.key, o, n),
            (None, Some(n)) => format!("{}: {}", self.key, n),
            (Some(o), None) => format!("{}: {}", self.key, o),
            (None, None) => self.key.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectDiff {
    pub mark: Mark,
    pub name: String,
    /// オブジェクトの中の位置（0 始まり）
    pub pos_a: Option<usize>,
    pub pos_b: Option<usize>,
    /// 最長共通部分列から外れた同名の効果を組にしたもの
    pub reordered: bool,
    pub items: Vec<ItemChange>,
}

/// 結果に出すためのオブジェクトの要約
#[derive(Debug, Clone)]
pub struct ObjectRef {
    /// `Project::objects` の中の位置
    pub index: usize,
    pub layer: usize,
    pub frames: Vec<usize>,
    pub kind: String,
    /// テキストなら本文の先頭、ファイルならファイル名
    pub label: String,
}

impl ObjectRef {
    fn of(index: usize, o: &Object) -> Self {
        let label = match o.identity() {
            Some(("テキスト", v)) => {
                let flat = v.replace("\\n", " ");
                let head: String = flat.chars().take(20).collect();
                if flat.chars().count() > 20 {
                    format!("「{head}…」")
                } else {
                    format!("「{head}」")
                }
            }
            Some((_, v)) if !v.is_empty() => {
                let name = v.rsplit(['\\', '/']).next().unwrap_or(v);
                format!("（{name}）")
            }
            _ => String::new(),
        };
        Self { index, layer: o.layer, frames: o.frames.clone(), kind: o.kind().to_string(), label }
    }

    /// 本体の表示に合わせてレイヤーは 1 始まり。フレームは `.aup2` の値のまま
    pub fn layer_text(&self) -> String {
        format!("L{}", self.layer + 1)
    }

    pub fn frame_text(&self) -> String {
        match (self.frames.first(), self.frames.last()) {
            (Some(s), Some(e)) => format!("{s}-{e}"),
            _ => "?".into(),
        }
    }

    pub fn heading(&self) -> String {
        format!("{} {} {}{}", self.layer_text(), self.frame_text(), self.kind, self.label)
    }
}

#[derive(Debug, Clone)]
pub struct ObjectDiff {
    pub mark: Mark,
    pub stage: Option<Stage>,
    pub a: Option<ObjectRef>,
    pub b: Option<ObjectRef>,
    /// オブジェクトの行の変化（`layer` / `frame` 以外）
    pub props: Vec<ItemChange>,
    pub effects: Vec<EffectDiff>,
}

impl ObjectDiff {
    fn sort_key(&self) -> (usize, usize) {
        let r = self.b.as_ref().or(self.a.as_ref()).expect("a か b のどちらかはある");
        (r.layer, r.frames.first().copied().unwrap_or(0))
    }

    pub fn moved(&self) -> bool {
        match (&self.a, &self.b) {
            (Some(a), Some(b)) => a.layer != b.layer || a.frames != b.frames,
            _ => false,
        }
    }

    /// 見出し。移動していれば `L3 → L5`、`120-239 → 150-269` の形で出す
    pub fn heading(&self) -> String {
        match (&self.a, &self.b) {
            (Some(a), Some(b)) if self.moved() => {
                let layer = if a.layer == b.layer {
                    b.layer_text()
                } else {
                    format!("{} → {}", a.layer_text(), b.layer_text())
                };
                let frame = if a.frames == b.frames {
                    b.frame_text()
                } else {
                    format!("{} → {}", a.frame_text(), b.frame_text())
                };
                format!("{layer} {frame} {}{}", b.kind, b.label)
            }
            (_, Some(b)) => b.heading(),
            (Some(a), None) => a.heading(),
            (None, None) => String::new(),
        }
    }

    /// 絞り込み（効果名・項目名・値・見出しのどれかに含まれる）
    pub fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let has = |s: &str| s.contains(needle);
        let item_has = |c: &ItemChange| {
            has(&c.key) || c.old.as_deref().is_some_and(has) || c.new.as_deref().is_some_and(has)
        };
        has(&self.heading())
            || self.props.iter().any(item_has)
            || self.effects.iter().any(|e| has(&e.name) || e.items.iter().any(item_has))
    }
}

#[derive(Debug, Clone)]
pub struct SceneDiff {
    pub id: i32,
    pub name: String,
    pub mark: Mark,
    pub settings: Vec<ItemChange>,
    pub objects: Vec<ObjectDiff>,
}

#[derive(Debug, Clone, Default)]
pub struct DiffResult {
    pub project: Vec<ItemChange>,
    pub scenes: Vec<SceneDiff>,
    /// `[plugin.N]` などの節（`show_ignored` のときだけ）
    pub others: Vec<(String, Vec<ItemChange>)>,
}

impl DiffResult {
    pub fn is_empty(&self) -> bool {
        self.project.is_empty() && self.scenes.is_empty() && self.others.is_empty()
    }

    /// (追加, 削除, 変更) のオブジェクト数
    pub fn object_counts(&self) -> (usize, usize, usize) {
        let mut n = (0, 0, 0);
        for o in self.scenes.iter().flat_map(|s| &s.objects) {
            match o.mark {
                Mark::Added => n.0 += 1,
                Mark::Removed => n.1 += 1,
                Mark::Changed => n.2 += 1,
            }
        }
        n
    }
}

// ---- 比べない行 ----

fn ignored_project_key(k: &str) -> bool {
    k == "file" || k.starts_with("display.") || k.starts_with("preview.")
}

/// シーンの行のうち、表示位置・カーソル・選択範囲（編集のたびに動く UI の状態）
fn ignored_scene_key(k: &str) -> bool {
    k.starts_with("cursor.")
        || k.starts_with("preview.")
        || matches!(k, "display.frame" | "display.layer" | "display.zoom" | "select.start" | "select.end")
}

fn ignored_object_key(k: &str) -> bool {
    k == "focus"
}

/// グループの開閉（`*.hide` と `Group` / `Group2` / `Group3`）
fn ignored_effect_key(k: &str) -> bool {
    k.ends_with(".hide") || matches!(k, "Group" | "Group2" | "Group3")
}

/// キーの順序を保って比べる。A にあるキーを A の順に、続けて B にだけあるキーを B の順に
fn diff_entries(a: &Entries, b: &Entries, ignore: impl Fn(&str) -> bool) -> Vec<ItemChange> {
    let index_b: HashMap<&str, &str> = b.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let index_a: HashMap<&str, &str> = a.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut out = Vec::new();
    for (k, v) in a {
        if ignore(k) {
            continue;
        }
        match index_b.get(k.as_str()) {
            Some(nv) if *nv == v => {}
            Some(nv) => out.push(ItemChange { key: k.clone(), old: Some(v.clone()), new: Some(nv.to_string()) }),
            None => out.push(ItemChange { key: k.clone(), old: Some(v.clone()), new: None }),
        }
    }
    for (k, v) in b {
        if ignore(k) || index_a.contains_key(k.as_str()) {
            continue;
        }
        out.push(ItemChange { key: k.clone(), old: None, new: Some(v.clone()) });
    }
    out
}

fn diff_effect_items(a: &Effect, b: &Effect, opt: Options) -> Vec<ItemChange> {
    let ignore = |k: &str| !opt.show_ignored && ignored_effect_key(k);
    let mut items = diff_entries(&a.entries, &b.entries, ignore);
    // effect.disable が無いのは有効（=0）と同じ
    items.retain(|c| {
        !(c.key == "effect.disable"
            && c.old.as_deref().unwrap_or("0") == c.new.as_deref().unwrap_or("0"))
    });
    items
}

/// 最長共通部分列の組（A の位置, B の位置）
fn lcs_pairs(a: &[&str], b: &[&str]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] { dp[i + 1][j + 1] + 1 } else { dp[i + 1][j].max(dp[i][j + 1]) };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < n && j < m {
        if a[i] == b[j] {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn diff_effects(a: &Object, b: &Object, opt: Options) -> Vec<EffectDiff> {
    let names_a = a.effect_names();
    let names_b = b.effect_names();
    let mut pairs: Vec<(usize, usize, bool)> = lcs_pairs(&names_a, &names_b).into_iter().map(|(i, j)| (i, j, false)).collect();
    let mut used_a: Vec<bool> = vec![false; names_a.len()];
    let mut used_b: Vec<bool> = vec![false; names_b.len()];
    for &(i, j, _) in &pairs {
        used_a[i] = true;
        used_b[j] = true;
    }
    // 残った同名の効果は並べ替えとみなす
    for i in 0..names_a.len() {
        if used_a[i] {
            continue;
        }
        if let Some(j) = (0..names_b.len()).find(|&j| !used_b[j] && names_b[j] == names_a[i]) {
            used_a[i] = true;
            used_b[j] = true;
            pairs.push((i, j, true));
        }
    }
    let mut out = Vec::new();
    for (i, j, reordered) in pairs {
        let items = diff_effect_items(&a.effects[i], &b.effects[j], opt);
        if items.is_empty() && !reordered {
            continue;
        }
        out.push(EffectDiff {
            mark: Mark::Changed,
            name: names_b[j].to_string(),
            pos_a: Some(i),
            pos_b: Some(j),
            reordered,
            items,
        });
    }
    for (i, name) in names_a.iter().enumerate().filter(|(i, _)| !used_a[*i]) {
        out.push(EffectDiff { mark: Mark::Removed, name: name.to_string(), pos_a: Some(i), pos_b: None, reordered: false, items: Vec::new() });
    }
    for (j, name) in names_b.iter().enumerate().filter(|(j, _)| !used_b[*j]) {
        out.push(EffectDiff { mark: Mark::Added, name: name.to_string(), pos_a: None, pos_b: Some(j), reordered: false, items: Vec::new() });
    }
    out.sort_by_key(|e| e.pos_b.or(e.pos_a).unwrap_or(0));
    out
}

fn overlap(a: &Object, b: &Object) -> usize {
    let (s, e) = (a.frame_start().max(b.frame_start()), a.frame_end().min(b.frame_end()));
    if e >= s {
        e - s + 1
    } else {
        0
    }
}

fn move_key(o: &Object) -> (String, String) {
    let id = match o.identity() {
        Some((k, v)) => format!("{k}={v}"),
        None => o.effect_names().join("\u{1}"),
    };
    (o.kind().to_string(), id)
}

/// シーン内のオブジェクトを組にする。`(A の添字, B の添字, 段)` と、残った A・B
fn pair_objects(a: &[&Object], b: &[&Object]) -> (Vec<(usize, usize, Stage)>, Vec<usize>, Vec<usize>) {
    let mut pairs = Vec::new();
    let mut free_b: Vec<bool> = vec![true; b.len()];
    let mut rest_a = Vec::new();

    // 1. 完全一致
    let mut exact: HashMap<(usize, &[usize], Vec<&str>), VecDeque<usize>> = HashMap::new();
    for (j, o) in b.iter().enumerate() {
        exact.entry((o.layer, o.frames.as_slice(), o.effect_names())).or_default().push_back(j);
    }
    for (i, o) in a.iter().enumerate() {
        match exact.get_mut(&(o.layer, o.frames.as_slice(), o.effect_names())).and_then(|q| q.pop_front()) {
            Some(j) => {
                free_b[j] = false;
                pairs.push((i, j, Stage::Exact));
            }
            None => rest_a.push(i),
        }
    }

    // 2. 移動 → 3. 変更
    type Fits = fn(&Object, &Object) -> bool;
    let moved: Fits = |x, y| move_key(x) == move_key(y);
    let modified: Fits = |x, y| x.layer == y.layer && x.kind() == y.kind() && overlap(x, y) > 0;
    for (stage, fits) in [(Stage::Moved, moved), (Stage::Modified, modified)] {
        let mut still = Vec::new();
        for i in rest_a {
            let best = (0..b.len())
                .filter(|&j| free_b[j] && fits(a[i], b[j]))
                .max_by_key(|&j| {
                    let o = overlap(a[i], b[j]);
                    let dl = a[i].layer.abs_diff(b[j].layer);
                    let df = a[i].frame_start().abs_diff(b[j].frame_start());
                    (o, std::cmp::Reverse(dl), std::cmp::Reverse(df), std::cmp::Reverse(j))
                });
            match best {
                Some(j) => {
                    free_b[j] = false;
                    pairs.push((i, j, stage));
                }
                None => still.push(i),
            }
        }
        rest_a = still;
    }
    let rest_b = (0..b.len()).filter(|&j| free_b[j]).collect();
    (pairs, rest_a, rest_b)
}

/// シーン内のオブジェクト（`Project::objects` の位置つき）
type SceneObjects<'a> = Vec<(usize, &'a Object)>;

fn scene_objects(p: &Project, scene: i32) -> SceneObjects<'_> {
    p.objects.iter().enumerate().filter(|(_, o)| o.scene == scene).collect()
}

fn diff_scene_objects(a: &SceneObjects, b: &SceneObjects, opt: Options) -> Vec<ObjectDiff> {
    let only_a: Vec<&Object> = a.iter().map(|(_, o)| *o).collect();
    let only_b: Vec<&Object> = b.iter().map(|(_, o)| *o).collect();
    let (pairs, rest_a, rest_b) = pair_objects(&only_a, &only_b);
    let mut out = Vec::new();
    for (i, j, stage) in pairs {
        let ((ia, oa), (ib, ob)) = (a[i], b[j]);
        let ignore = |k: &str| !opt.show_ignored && ignored_object_key(k);
        let props = diff_entries(&oa.entries, &ob.entries, ignore);
        let effects = diff_effects(oa, ob, opt);
        let d = ObjectDiff {
            mark: Mark::Changed,
            stage: Some(stage),
            a: Some(ObjectRef::of(ia, oa)),
            b: Some(ObjectRef::of(ib, ob)),
            props,
            effects,
        };
        if d.props.is_empty() && d.effects.is_empty() && !d.moved() {
            continue;
        }
        out.push(d);
    }
    for i in rest_a {
        let (ia, oa) = a[i];
        out.push(ObjectDiff { mark: Mark::Removed, stage: None, a: Some(ObjectRef::of(ia, oa)), b: None, props: Vec::new(), effects: Vec::new() });
    }
    for j in rest_b {
        let (ib, ob) = b[j];
        out.push(ObjectDiff { mark: Mark::Added, stage: None, a: None, b: Some(ObjectRef::of(ib, ob)), props: Vec::new(), effects: Vec::new() });
    }
    out.sort_by_key(|d| d.sort_key());
    out
}

/// `from` の `index` 番目のオブジェクトが、`to` のどれに当たるか（同じシーンの中で対応付ける）。
/// 項目の履歴で、バックアップを 1 本ずつたどるのに使う
pub fn counterpart(from: &Project, index: usize, to: &Project) -> Option<usize> {
    let scene = from.objects.get(index)?.scene;
    let a = scene_objects(from, scene);
    let b = scene_objects(to, scene);
    let pos = a.iter().position(|(i, _)| *i == index)?;
    let only_a: Vec<&Object> = a.iter().map(|(_, o)| *o).collect();
    let only_b: Vec<&Object> = b.iter().map(|(_, o)| *o).collect();
    let (pairs, _, _) = pair_objects(&only_a, &only_b);
    pairs.into_iter().find(|(i, _, _)| *i == pos).map(|(_, j, _)| b[j].0)
}

pub fn diff(a: &Project, b: &Project, opt: Options) -> DiffResult {
    let mut result = DiffResult::default();
    if opt.only_scene.is_none() {
        result.project = diff_entries(&a.header, &b.header, |k| !opt.show_ignored && ignored_project_key(k));
    }

    let mut ids: Vec<i32> = a.scenes.iter().chain(&b.scenes).map(|s| s.id).collect();
    ids.extend(a.objects.iter().chain(&b.objects).map(|o| o.scene));
    ids.sort();
    ids.dedup();
    if let Some(only) = opt.only_scene {
        ids.retain(|id| *id == only);
    }
    for id in ids {
        let sa = a.scenes.iter().find(|s| s.id == id);
        let sb = b.scenes.iter().find(|s| s.id == id);
        let name = b.scene_name(id).or_else(|| a.scene_name(id)).map(str::to_string).unwrap_or_else(|| format!("シーン {id}"));
        let empty = Entries::new();
        let settings = if opt.only_scene.is_some() {
            Vec::new()
        } else {
            diff_entries(
                sa.map(|s| &s.entries).unwrap_or(&empty),
                sb.map(|s| &s.entries).unwrap_or(&empty),
                |k| !opt.show_ignored && ignored_scene_key(k),
            )
        };
        let objects = diff_scene_objects(&scene_objects(a, id), &scene_objects(b, id), opt);
        let mark = match (sa, sb) {
            _ if opt.only_scene.is_some() => Mark::Changed,
            (None, Some(_)) => Mark::Added,
            (Some(_), None) => Mark::Removed,
            _ => Mark::Changed,
        };
        if mark == Mark::Changed && settings.is_empty() && objects.is_empty() {
            continue;
        }
        result.scenes.push(SceneDiff { id, name, mark, settings, objects });
    }

    if opt.show_ignored && opt.only_scene.is_none() {
        let empty = Entries::new();
        let mut names: Vec<&str> = a.others.iter().chain(&b.others).map(|(n, _)| n.as_str()).collect();
        names.sort();
        names.dedup();
        for n in names {
            let ea = a.others.iter().find(|(x, _)| x == n).map(|(_, e)| e).unwrap_or(&empty);
            let eb = b.others.iter().find(|(x, _)| x == n).map(|(_, e)| e).unwrap_or(&empty);
            let changes = diff_entries(ea, eb, |_| false);
            if !changes.is_empty() {
                result.others.push((n.to_string(), changes));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aup2::parse;

    /// テスト用の `.aup2` を組み立てる。オブジェクトは (レイヤー, 開始, 終了, 効果の列) で、
    /// 効果は (効果名, 行) 。ホストと同じく (レイヤー, 開始) の順に番号を振る
    struct Obj<'a> {
        layer: usize,
        start: usize,
        end: usize,
        extra: &'a [(&'a str, &'a str)],
        effects: Vec<(&'a str, Vec<(&'a str, &'a str)>)>,
    }

    fn obj<'a>(layer: usize, start: usize, end: usize, effects: Vec<(&'a str, Vec<(&'a str, &'a str)>)>) -> Obj<'a> {
        Obj { layer, start, end, extra: &[], effects }
    }

    fn build(objs: &[Obj]) -> String {
        let mut order: Vec<&Obj> = objs.iter().collect();
        order.sort_by_key(|o| (o.layer, o.start));
        let mut s = String::from("[project]\r\nversion=2011200\r\nfile=C:\\p.aup2\r\n[scene.0]\r\nscene=0\r\nname=Root\r\ncursor.frame=0\r\n");
        for (n, o) in order.iter().enumerate() {
            s += &format!("[{n}]\r\nlayer={}\r\n", o.layer);
            for (k, v) in o.extra {
                s += &format!("{k}={v}\r\n");
            }
            s += &format!("frame={},{}\r\n", o.start, o.end);
            for (m, (name, items)) in o.effects.iter().enumerate() {
                s += &format!("[{n}.{m}]\r\neffect.name={name}\r\n");
                for (k, v) in items {
                    s += &format!("{k}={v}\r\n");
                }
            }
        }
        s
    }

    fn text<'a>(body: &'a str) -> Vec<(&'a str, Vec<(&'a str, &'a str)>)> {
        vec![("テキスト", vec![("サイズ", "64"), ("テキスト", body)]), ("標準描画", vec![("X", "0.00"), ("Group", "1")])]
    }

    fn base<'a>() -> Vec<Obj<'a>> {
        vec![
            obj(0, 0, 59, text("一")),
            obj(0, 60, 119, text("二")),
            obj(1, 0, 119, vec![("図形", vec![("色", "ff0000")]), ("ぼかし", vec![("範囲", "5")]), ("縁取り", vec![]), ("標準描画", vec![])]),
            obj(2, 30, 89, vec![("画像ファイル", vec![("ファイル", "D:\\素材\\bg.png")]), ("標準描画", vec![])]),
        ]
    }

    fn run(a: &[Obj], b: &[Obj]) -> DiffResult {
        diff(&parse(&build(a)), &parse(&build(b)), Options::default())
    }

    fn only_object(r: &DiffResult) -> &ObjectDiff {
        assert_eq!(r.scenes.len(), 1, "{r:#?}");
        assert_eq!(r.scenes[0].objects.len(), 1, "{:#?}", r.scenes[0].objects);
        &r.scenes[0].objects[0]
    }

    #[test]
    fn identical_projects_have_no_diff() {
        assert!(run(&base(), &base()).is_empty());
    }

    #[test]
    fn inserted_object_shifts_numbers_but_shows_one_addition() {
        let mut b = base();
        // レイヤー 0 の先頭に挿入すると、後ろの番号が全部ずれる
        b.push(obj(0, 200, 259, text("三")));
        b.push(obj(0, 120, 199, text("挿入")));
        let mut a = base();
        a.push(obj(0, 200, 259, text("三")));
        let r = run(&a, &b);
        let o = only_object(&r);
        assert_eq!(o.mark, Mark::Added);
        assert_eq!(o.b.as_ref().unwrap().label, "「挿入」");
        assert_eq!(r.object_counts(), (1, 0, 0));
    }

    #[test]
    fn moved_object_shows_one_move() {
        let mut b = base();
        b[3] = obj(4, 50, 109, b[3].effects.clone());
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!((o.mark, o.stage), (Mark::Changed, Some(Stage::Moved)));
        assert!(o.effects.is_empty() && o.props.is_empty());
        assert_eq!(o.heading(), "L3 → L5 30-89 → 50-109 画像ファイル（bg.png）");
    }

    #[test]
    fn empty_file_has_no_label() {
        let mut b = base();
        b.push(obj(5, 0, 9, vec![("画像ファイル", vec![("ファイル", "")])]));
        let r = run(&base(), &b);
        assert_eq!(only_object(&r).heading(), "L6 0-9 画像ファイル");
    }

    #[test]
    fn changed_value_in_place_is_exact_match() {
        let mut b = base();
        b[0] = obj(0, 0, 59, text("壱"));
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(o.stage, Some(Stage::Exact));
        assert_eq!(o.effects.len(), 1);
        assert_eq!(o.effects[0].items, [ItemChange { key: "テキスト".into(), old: Some("一".into()), new: Some("壱".into()) }]);
    }

    #[test]
    fn text_changed_and_resized_is_modified() {
        let mut b = base();
        b[1] = obj(0, 60, 149, text("2"));
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(o.stage, Some(Stage::Modified));
        assert_eq!(o.heading(), "L1 60-119 → 60-149 テキスト「2」");
    }

    #[test]
    fn added_effect_is_one_change() {
        let mut b = base();
        b[2].effects.insert(2, ("発光", vec![("強さ", "50")]));
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(o.effects.len(), 1);
        let e = &o.effects[0];
        assert_eq!((e.mark, e.name.as_str(), e.pos_b), (Mark::Added, "発光", Some(2)));
    }

    #[test]
    fn reordered_effect_is_one_change() {
        let mut b = base();
        b[2].effects.swap(1, 2); // ぼかし と 縁取り を入れ替え
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(o.stage, Some(Stage::Modified), "効果の列が変わるので、完全一致でも移動でもない");
        assert_eq!(o.effects.len(), 1, "{:#?}", o.effects);
        assert!(o.effects[0].reordered);
    }

    #[test]
    fn disabled_effect_is_one_change() {
        let mut b = base();
        b[2].effects[1].1.insert(0, ("effect.disable", "1"));
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(o.effects.len(), 1);
        assert_eq!(o.effects[0].items, [ItemChange { key: "effect.disable".into(), old: None, new: Some("1".into()) }]);
        // 明示的な 0 と、行が無いのは同じ
        let mut c = base();
        c[2].effects[1].1.insert(0, ("effect.disable", "0"));
        assert!(run(&base(), &c).is_empty());
    }

    #[test]
    fn focus_and_group_state_are_ignored() {
        let mut b = base();
        b[0].extra = &[("focus", "1")];
        b[2].effects[1].1.push(("範囲.hide", "1"));
        b[0].effects[1].1[1] = ("Group", "0");
        let text_b = build(&b).replace("cursor.frame=0", "cursor.frame=99");
        let (pa, pb) = (parse(&build(&base())), parse(&text_b));
        assert!(diff(&pa, &pb, Options::default()).is_empty());
        let shown = diff(&pa, &pb, Options { show_ignored: true, ..Default::default() });
        assert!(!shown.is_empty());
        assert_eq!(shown.scenes[0].settings.len(), 1, "cursor.frame");
    }

    #[test]
    fn removed_object_and_scene_settings() {
        let mut b = base();
        b.remove(1);
        let text_b = build(&b).replace("name=Root", "name=本編");
        let r = diff(&parse(&build(&base())), &parse(&text_b), Options::default());
        assert_eq!(r.scenes[0].name, "本編");
        assert_eq!(r.scenes[0].settings[0].text(), "name: Root → 本編");
        assert_eq!(r.object_counts(), (0, 1, 0));
    }

    #[test]
    fn filter_matches_effect_item_and_value() {
        let mut b = base();
        b[0] = obj(0, 0, 59, text("壱"));
        let r = run(&base(), &b);
        let o = only_object(&r);
        for needle in ["テキスト", "壱", "一", "L1", ""] {
            assert!(o.matches(needle), "{needle}");
        }
        assert!(!o.matches("ぼかし"));
    }

    #[test]
    fn only_scene_skips_project_and_settings() {
        let mut b = base();
        b[0] = obj(0, 0, 59, text("壱"));
        let text_b = build(&b).replace("name=Root", "name=本編").replace("version=2011200", "version=9");
        let r = diff(&parse(&build(&base())), &parse(&text_b), Options { only_scene: Some(0), ..Default::default() });
        assert!(r.project.is_empty());
        assert!(r.scenes[0].settings.is_empty());
        assert_eq!(r.object_counts(), (0, 0, 1));
        assert!(diff(&parse(&build(&base())), &parse(&text_b), Options { only_scene: Some(3), ..Default::default() }).is_empty());
    }

    #[test]
    fn counterpart_follows_renumbered_object() {
        let mut b = base();
        b.push(obj(0, 120, 179, text("挿入")));
        b[3] = obj(4, 50, 109, b[3].effects.clone());
        let (pa, pb) = (parse(&build(&base())), parse(&build(&b)));
        // A の画像（番号 3）は、B ではレイヤーが変わって番号 4 になる
        let ia = pa.objects.iter().position(|o| o.kind() == "画像ファイル").unwrap();
        let ib = counterpart(&pa, ia, &pb).unwrap();
        assert_eq!((pb.objects[ib].kind(), pb.objects[ib].layer), ("画像ファイル", 4));
        assert_eq!(counterpart(&pb, ib, &pa), Some(ia));
        let inserted = pb.objects.iter().position(|o| o.identity() == Some(("テキスト", "挿入"))).unwrap();
        assert_eq!(counterpart(&pb, inserted, &pa), None);
    }

    #[test]
    fn object_ref_index_points_into_project() {
        let mut b = base();
        b[0] = obj(0, 0, 59, text("壱"));
        let pb = parse(&build(&b));
        let r = run(&base(), &b);
        let o = only_object(&r);
        assert_eq!(pb.objects[o.b.as_ref().unwrap().index].identity(), Some(("テキスト", "壱")));
    }

    #[test]
    fn lcs_handles_duplicates() {
        assert_eq!(lcs_pairs(&["a", "b", "a"], &["a", "a"]), [(0, 0), (2, 1)]);
        assert!(lcs_pairs(&[], &["a"]).is_empty());
    }
}
