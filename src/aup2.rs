//! `.aup2` のパーサー（読むだけ）
//!
//! ColorHistory_H の `parse_aup2` を土台に、全キーを順序つきで持つ形へ広げたもの。
//! 形式は `.claude/rules/au2-aup2-format.md`。
//!
//! - `[project]` / `[scene.N]` / `[N]`（オブジェクト）/ `[N.M]`（効果）/ それ以外（`[plugin.N]` など）を分ける
//! - オブジェクトの `scene=` が無ければシーン 0
//! - 壊れた行（`=` の無い行、閉じない見出し、持ち主のいない効果）は数えて読み飛ばす

pub type Entries = Vec<(String, String)>;

#[derive(Debug, Default, Clone)]
pub struct Project {
    /// `[project]` の行
    pub header: Entries,
    pub scenes: Vec<Scene>,
    pub objects: Vec<Object>,
    /// `[plugin.N]` など、上のどれでもない節（見出しと行）
    pub others: Vec<(String, Entries)>,
    /// 読み飛ばした行の数
    pub broken_lines: usize,
}

#[derive(Debug, Default, Clone)]
pub struct Scene {
    pub id: i32,
    pub entries: Entries,
}

#[derive(Debug, Default, Clone)]
pub struct Object {
    /// `[N]` の N（並び替えで振り直されるので、対応付けには使わない）
    pub number: usize,
    pub scene: i32,
    pub layer: usize,
    /// `frame=` の値（開始, 中間点…, 終了）
    pub frames: Vec<usize>,
    /// `layer` / `scene` / `frame` 以外のオブジェクトの行（`focus=` など）
    pub entries: Entries,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Default, Clone)]
pub struct Effect {
    pub name: String,
    /// `effect.name` 以外の行（`effect.disable` を含む）
    pub entries: Entries,
}

impl Project {
    pub fn scene_name(&self, id: i32) -> Option<&str> {
        self.scenes.iter().find(|s| s.id == id).and_then(|s| lookup(&s.entries, "name"))
    }
}

impl Object {
    pub fn frame_start(&self) -> usize {
        self.frames.first().copied().unwrap_or(0)
    }

    pub fn frame_end(&self) -> usize {
        self.frames.last().copied().unwrap_or(0)
    }

    /// 先頭の効果名（メディアの種類）
    pub fn kind(&self) -> &str {
        self.effects.first().map(|e| e.name.as_str()).unwrap_or("")
    }

    pub fn effect_names(&self) -> Vec<&str> {
        self.effects.iter().map(|e| e.name.as_str()).collect()
    }

    /// 先頭の効果の `テキスト=` か `ファイル=`
    pub fn identity(&self) -> Option<(&'static str, &str)> {
        let first = self.effects.first()?;
        ["テキスト", "ファイル"]
            .into_iter()
            .find_map(|k| lookup(&first.entries, k).map(|v| (k, v)))
    }
}

pub fn lookup<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

enum Section {
    Project,
    Scene(usize),
    Object(usize),
    Effect(usize),
    Other(usize),
    /// 読み飛ばす（持ち主のいない効果など）
    Skip,
}

pub fn parse(text: &str) -> Project {
    let mut p = Project::default();
    let mut section = Section::Skip;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) else {
                p.broken_lines += 1;
                section = Section::Skip;
                continue;
            };
            section = open_section(&mut p, name);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            p.broken_lines += 1;
            continue;
        };
        let entry = (key.to_string(), value.to_string());
        match section {
            Section::Project => p.header.push(entry),
            Section::Scene(i) => p.scenes[i].entries.push(entry),
            Section::Object(i) => {
                let o = &mut p.objects[i];
                match key {
                    "layer" => o.layer = value.trim().parse().unwrap_or(0),
                    "scene" => o.scene = value.trim().parse().unwrap_or(0),
                    "frame" => o.frames = value.split(',').filter_map(|f| f.trim().parse().ok()).collect(),
                    _ => o.entries.push(entry),
                }
            }
            Section::Effect(i) => {
                if let Some(e) = p.objects[i].effects.last_mut() {
                    if key == "effect.name" {
                        e.name = value.to_string();
                    } else {
                        e.entries.push(entry);
                    }
                }
            }
            Section::Other(i) => p.others[i].1.push(entry),
            Section::Skip => p.broken_lines += 1,
        }
    }
    p
}

fn open_section(p: &mut Project, name: &str) -> Section {
    if name == "project" {
        return Section::Project;
    }
    if let Some(n) = name.strip_prefix("scene.") {
        if let Ok(id) = n.parse::<i32>() {
            p.scenes.push(Scene { id, entries: Vec::new() });
            return Section::Scene(p.scenes.len() - 1);
        }
    }
    if let Ok(n) = name.parse::<usize>() {
        p.objects.push(Object { number: n, ..Default::default() });
        return Section::Object(p.objects.len() - 1);
    }
    if let Some((obj, eff)) = name.split_once('.') {
        if let (Ok(o), Ok(_)) = (obj.parse::<usize>(), eff.parse::<usize>()) {
            // 効果は直前のオブジェクトの直後に並ぶ（ホストの正規形）
            return match p.objects.iter().rposition(|x| x.number == o) {
                Some(i) => {
                    p.objects[i].effects.push(Effect::default());
                    Section::Effect(i)
                }
                None => {
                    p.broken_lines += 1;
                    Section::Skip
                }
            };
        }
    }
    p.others.push((name.to_string(), Vec::new()));
    Section::Other(p.others.len() - 1)
}

/// 本体の API（`get_alias`）が返すオブジェクトのエイリアスを、`.aup2` のオブジェクトと同じ形にする。
///
/// エイリアスは `[Object]`（オブジェクトの行）と `[Object.N]`（効果）でできている（ColorHistory_H の実例）。
/// レイヤーと開始・終了は API の値（`get_layer_frame`）を使う。エイリアスの `frame=` は中間点を取るためだけに読み、
/// 先頭が開始と合わなければ（オブジェクトの先頭からの相対値なら）開始の分だけずらす
pub fn parse_alias(text: &str, scene: i32, layer: usize, start: usize, end: usize) -> Object {
    let mut o = Object { number: 0, scene, layer, frames: vec![start, end], ..Default::default() };
    let mut in_effect = false;
    let mut in_object = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_object = name == "Object";
            in_effect = name.strip_prefix("Object.").is_some_and(|n| n.parse::<usize>().is_ok());
            if in_effect {
                o.effects.push(Effect::default());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if in_effect {
            if let Some(e) = o.effects.last_mut() {
                if key == "effect.name" {
                    e.name = value.to_string();
                } else {
                    e.entries.push((key.to_string(), value.to_string()));
                }
            }
        } else if in_object {
            match key {
                "frame" => {
                    let f: Vec<usize> = value.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if f.len() >= 2 {
                        let shift = if f[0] == start { 0 } else { start.saturating_sub(f[0]) };
                        o.frames = f.iter().map(|x| x + shift).collect();
                        if let Some(last) = o.frames.last_mut() {
                            *last = end;
                        }
                    }
                }
                "layer" | "scene" => {}
                _ => o.entries.push((key.to_string(), value.to_string())),
            }
        }
    }
    o
}

/// 一覧用の軽い読み取り。全体を構造にせず、`file=` とシーン数・オブジェクト数だけを数える。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub file: String,
    pub scenes: usize,
    pub objects: usize,
}

pub fn summarize(text: &str) -> Summary {
    let mut s = Summary::default();
    let mut in_project = false;
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(name) = line.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
            in_project = name == "project";
            if name.starts_with("scene.") {
                s.scenes += 1;
            } else if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
                s.objects += 1;
            }
            continue;
        }
        if in_project {
            if let Some(v) = line.strip_prefix("file=") {
                s.file = v.to_string();
            }
        }
    }
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const SAMPLE: &str = "[project]\r\nversion=2011200\r\nfile=C:\\a\\b.aup2\r\ndisplay.scene=0\r\n\
[scene.0]\r\nscene=0\r\nname=Root\r\nvideo.width=1920\r\ncursor.frame=10\r\n\
[0]\r\nlayer=0\r\nfocus=1\r\nframe=0,59\r\n[0.0]\r\neffect.name=テキスト\r\nサイズ=64\r\nテキスト=言葉が\\n流れる\r\n[0.1]\r\neffect.name=標準描画\r\nX=0.00\r\n\
[1]\r\nlayer=2\r\nframe=30,45,60\r\n[1.0]\r\neffect.name=図形\r\n色=ff0000\r\n[1.1]\r\neffect.name=ぼかし\r\neffect.disable=1\r\n範囲=5\r\n[1.2]\r\neffect.name=標準描画\r\n\
[scene.1]\r\nscene=1\r\nname=サブ\r\n\
[2]\r\nlayer=0\r\nscene=1\r\nframe=0,9\r\n[2.0]\r\neffect.name=画像ファイル\r\nファイル=D:\\素材\\bg.png\r\n\
[plugin.0]\r\nplugin.name=Foo\r\n";

    #[test]
    fn parses_sections_in_order() {
        let p = parse(SAMPLE);
        assert_eq!(lookup(&p.header, "version"), Some("2011200"));
        assert_eq!(p.scenes.len(), 2);
        assert_eq!(p.scene_name(1), Some("サブ"));
        assert_eq!(p.objects.len(), 3);
        let o = &p.objects[1];
        assert_eq!((o.scene, o.layer, o.frame_start(), o.frame_end()), (0, 2, 30, 60));
        assert_eq!(o.effect_names(), ["図形", "ぼかし", "標準描画"]);
        assert_eq!(lookup(&o.effects[1].entries, "effect.disable"), Some("1"));
        assert_eq!(p.objects[0].entries, [("focus".to_string(), "1".to_string())]);
        assert_eq!(p.objects[2].scene, 1);
        assert_eq!(p.objects[2].identity(), Some(("ファイル", "D:\\素材\\bg.png")));
        assert_eq!(p.objects[0].identity(), Some(("テキスト", "言葉が\\n流れる")));
        assert_eq!(p.others.len(), 1);
        assert_eq!(p.broken_lines, 0);
    }

    #[test]
    fn alias_becomes_object() {
        let alias = "[Object]\r\nframe=0,30,59\r\nfocus=1\r\n[Object.0]\r\neffect.name=テキスト\r\nテキスト=abc\r\n[Object.1]\r\neffect.name=縁取り\r\neffect.disable=1\r\n[Object.2]\r\neffect.name=標準描画\r\nX=0.00\r\n";
        let o = parse_alias(alias, 1, 4, 120, 179);
        assert_eq!((o.scene, o.layer), (1, 4));
        assert_eq!(o.frames, [120, 150, 179], "相対の frame= を開始の分だけずらす");
        assert_eq!(o.effect_names(), ["テキスト", "縁取り", "標準描画"]);
        assert_eq!(o.identity(), Some(("テキスト", "abc")));
        assert_eq!(o.entries, [("focus".to_string(), "1".to_string())]);
        // 絶対値の frame= はそのまま。frame= が無ければ開始と終了だけ
        assert_eq!(parse_alias("[Object]\nframe=120,179\n", 0, 0, 120, 179).frames, [120, 179]);
        assert_eq!(parse_alias("[Object.0]\neffect.name=図形\n", 0, 0, 5, 9).frames, [5, 9]);
    }

    #[test]
    fn summary_counts_scenes_and_objects() {
        let s = summarize(SAMPLE);
        assert_eq!(s, Summary { file: "C:\\a\\b.aup2".into(), scenes: 2, objects: 3 });
    }

    #[test]
    fn broken_input_does_not_panic() {
        let text = "[project\r\nfile=x\r\nno equals here\r\n[5.0]\r\neffect.name=孤児\r\n[0]\r\nlayer=x\r\nframe=a,b\r\n[0.0]\r\neffect.name=テキスト\r\nテキスト=途中で切れ";
        let p = parse(text);
        assert_eq!(p.objects.len(), 1);
        assert_eq!(p.objects[0].layer, 0);
        assert!(p.objects[0].frames.is_empty());
        assert_eq!(p.objects[0].effects[0].name, "テキスト");
        // 閉じない見出し・= の無い行・持ち主のいない効果の見出しと中身
        assert!(p.broken_lines >= 4, "broken_lines = {}", p.broken_lines);
        let _ = summarize(text);
        let _ = parse("");
        let _ = parse("\u{feff}[project]\nfile=");
    }
}
