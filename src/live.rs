//! 本体とのやり取り（第 2 段階）
//!
//! - **読み取りは `call_read_section`**。「比較」を押したときに、表示中のシーンだけを読む（他のシーンは API で読めない）
//! - **書き込みは「戻す」を押したときだけ `call_edit_section`**。1 回押すと本体の Undo 1 回分。
//!   イベント・タイマー・フォーカスの変化からは呼ばない（本体が UI 操作中の Undo を捨てる。`.claude/rules/au2-rs-plugin.md`）
//! - 書く前に今の値を読み直し、比べたときの値と同じでなければ書かない。書いた後に読み返して確かめる

use aviutl2::generic::{GlobalEditHandle, ObjectHandle};

use crate::aup2::{self, Project, Scene};

pub static EDIT_HANDLE: GlobalEditHandle = GlobalEditHandle::new();

/// 表示中のシーンを `.aup2` と同じ形にしたもの
pub struct LiveScene {
    pub project: Project,
    /// `project.objects` と同じ並びのハンドル
    pub handles: Vec<ObjectHandle>,
    pub scene_id: i32,
    pub scene_name: String,
    /// エイリアスを読めなかったオブジェクトの数
    pub unreadable: usize,
}

type RawObject = (ObjectHandle, usize, usize, usize, String);

/// 表示中のシーンを読む。`base`（比べる相手）のプロジェクトとシーンの行を写して、シーンの設定の違いが出ないようにする
pub fn read_current_scene(base: &Project) -> Result<LiveScene, String> {
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません".into());
    }
    // ReadSection は編集情報を持たないので、読み取りセクションに入る前に取る（ColorHistory_H と同じ）
    let info = EDIT_HANDLE.get_edit_info();
    let (scene_id, layer_max) = (info.scene_id, info.layer_max);
    let (scene_name, raw, unreadable) = EDIT_HANDLE
        .call_read_section(move |read| {
            let name = read.get_scene_name().unwrap_or_default();
            let mut out: Vec<RawObject> = Vec::new();
            let mut unreadable = 0;
            for layer in 0..=layer_max {
                for (lf, handle) in read.objects_in_layer(layer) {
                    match read.object(handle).get_alias() {
                        Ok(alias) => out.push((handle, lf.layer, lf.start, lf.end, alias)),
                        Err(_) => unreadable += 1,
                    }
                }
            }
            (name, out, unreadable)
        })
        .map_err(|e| format!("表示中のシーンを読めませんでした: {e:?}"))?;

    let mut project = Project { header: base.header.clone(), ..Default::default() };
    let entries = base
        .scenes
        .iter()
        .find(|s| s.id == scene_id)
        .map(|s| s.entries.clone())
        .unwrap_or_else(|| vec![("name".into(), scene_name.clone())]);
    project.scenes.push(Scene { id: scene_id, entries });
    let mut handles = Vec::with_capacity(raw.len());
    for (handle, layer, start, end, alias) in raw {
        project.objects.push(aup2::parse_alias(&alias, scene_id, layer, start, end));
        handles.push(handle);
    }
    Ok(LiveScene { project, handles, scene_id, scene_name, unreadable })
}

/// 戻す中身
#[derive(Debug, Clone)]
pub enum Restore {
    /// 設定項目の値（`expected` は比べたときの今の値）
    Item { key: String, expected: String, value: String },
    /// 効果の有効・無効
    Enable { expected: bool, value: bool },
}

/// トラックバーの値の、移動方法の前に並ぶ数値の数と、設定の先頭（ビット）。移動の無い値（数値 1 つ）や
/// トラックバーでない値は None
fn track_shape(value: &str) -> Option<(usize, &str, u32)> {
    let fields: Vec<&str> = value.split(',').map(str::trim).collect();
    let n = fields.iter().take_while(|f| f.parse::<f64>().is_ok()).count();
    if n == 0 || n == fields.len() {
        return None;
    }
    let bits = fields.get(n + 1).and_then(|s| s.split('|').next()).and_then(|b| b.parse::<u32>().ok()).unwrap_or(0);
    Some((n, fields[n], bits))
}

/// 戻す値の数値の数が、今のオブジェクトの点（開始・中間点・終了）の数と合うか。
/// 本体は数を検査せずに保存し、合わないと動きが変わる（ルール `au2-rs-plugin`「設定項目の値の形式」）。
/// 中間点無視（設定のビット 4）と再生範囲は値 2 つで保存されるので、2 つなら合うとみなす
pub fn values_fit_points(value: &str, points: usize) -> bool {
    match track_shape(value) {
        None => true,
        Some((n, motion, bits)) => n == points || (n == 2 && (bits & 4 != 0 || motion == "再生範囲")),
    }
}

/// 「戻す」ボタン専用。`pos` はオブジェクトの中の効果の位置（0 始まり）。
/// `scene_id` は比較したときのシーン（比べた後にシーンを切り替えていたら書かない）
pub fn restore(
    handle: ObjectHandle,
    scene_id: Option<i32>,
    pos: usize,
    effect_name: String,
    what: Restore,
) -> Result<String, String> {
    if !EDIT_HANDLE.is_ready() {
        return Err("編集 API の準備ができていません".into());
    }
    EDIT_HANDLE
        .call_edit_section(move |edit| -> Result<String, String> {
            if scene_id.is_some_and(|id| id != edit.info.scene_id) {
                return Err("比べたときとシーンが違います。比べたシーンに戻すか、もう一度比較してください".into());
            }
            let object = edit.object(handle);
            if !object.exists() {
                return Err("オブジェクトが見つかりません。もう一度比較してください".into());
            }
            let effects = object.get_effects().map_err(|e| format!("効果を読めませんでした: {e:?}"))?;
            let effect_handle = *effects
                .get(pos)
                .ok_or_else(|| format!("{} 番目の効果がありません。もう一度比較してください", pos + 1))?;
            let effect = edit.effect(effect_handle);
            let name = effect.get_name().map_err(|e| format!("効果名を読めませんでした: {e:?}"))?;
            if name != effect_name {
                return Err(format!("{} 番目の効果が「{name}」に変わっています。もう一度比較してください", pos + 1));
            }
            match what {
                Restore::Item { key, expected, value } => {
                    let current = effect.get_item_value(&key).map_err(|e| format!("{key} を読めませんでした: {e:?}"))?;
                    if current != expected {
                        return Err(format!("{key} の今の値（{current}）が比べたときと違うので、戻しませんでした。もう一度比較してください"));
                    }
                    let points = edit.get_object_section_num(handle).map_err(|e| format!("中間点の数を読めませんでした: {e:?}"))? + 1;
                    if !values_fit_points(&value, points) {
                        return Err(format!(
                            "{key} は中間点の数が違うので戻せません（戻す値の点 {} 個 / 今のオブジェクト {points} 個）。中間点をそろえてから戻してください",
                            track_shape(&value).map_or(1, |s| s.0)
                        ));
                    }
                    effect.set_item_value(&key, &value).map_err(|e| {
                        tracing::warn!(effect = %effect_name, item = %key, error = ?e, "restore: set failed");
                        format!("{key} に書き込めませんでした: {e:?}")
                    })?;
                    let written = effect.get_item_value(&key).map_err(|e| format!("{key} を読み返せませんでした: {e:?}"))?;
                    if written != value {
                        tracing::warn!(effect = %effect_name, item = %key, wrote = %value, read_back = %written, "restore: value did not stick");
                        return Err(format!("{key} に書き込みが反映されませんでした。今の値: {written}"));
                    }
                    Ok(format!("{effect_name} の {key} を {value} に戻しました"))
                }
                Restore::Enable { expected, value } => {
                    let current = effect.get_enable().map_err(|e| format!("有効・無効を読めませんでした: {e:?}"))?;
                    if current != expected {
                        return Err("有効・無効が比べたときと違うので、戻しませんでした。もう一度比較してください".into());
                    }
                    effect.set_enable(value).map_err(|e| format!("有効・無効を変えられませんでした: {e:?}"))?;
                    if effect.get_enable().ok() != Some(value) {
                        tracing::warn!(effect = %effect_name, "restore: enable did not stick");
                        return Err("有効・無効の変更が反映されませんでした".into());
                    }
                    Ok(format!("{effect_name} を{}に戻しました", if value { "有効" } else { "無効" }))
                }
            }
        })
        .map_err(|e| format!("編集 API エラー: {e:?}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_and_points() {
        // 移動の無い値・トラックバーでない値はいつでも書ける
        assert!(values_fit_points("100.00", 4));
        assert!(values_fit_points("通常", 4));
        assert!(values_fit_points("0.00,50.00,100.00,直線移動,0", 3));
        assert!(!values_fit_points("0.00,50.00,100.00,直線移動,0", 4));
        assert!(!values_fit_points("0.00,100.00,直線移動,0", 3));
        // 中間点無視と再生範囲は値 2 つ
        assert!(values_fit_points("0.00,100.00,直線移動,4", 5));
        assert!(values_fit_points("0.00,100.00,直線移動,6|", 5));
        assert!(values_fit_points("0.00,100.00,再生範囲,0", 3));
        // 設定の中のカンマ
        assert!(values_fit_points("0.00,100.00,プローブ移動_H,0|9,0,1", 2));
    }
}
