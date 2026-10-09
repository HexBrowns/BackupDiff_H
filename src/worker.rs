//! 裏のスレッドの起こし方と止め方
//!
//! 本体は終了時に DLL を外す。外れた後にスレッドが 1 本でも動いていると落ち、`aviutl2.ini` が保存されない
//! （`.claude/rules/au2-rs-plugin.md`「裏のスレッドは、プラグインが外れる前に止めて終わりを待つ」）。
//! スレッドはここからだけ起こし、プラグイン本体の `Drop` で `shutdown` を呼んで止めて待つ。
//! スレッドの側は 1 ファイル読むごとに `stopping()` を見る（1 ファイルは数 ms）

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

static STOP: AtomicBool = AtomicBool::new(false);
static THREADS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// 待つ上限。超えたら警告を出して先へ進む（DLL を外させないために待ち続けはしない）
const JOIN_LIMIT: Duration = Duration::from_secs(3);

pub fn spawn(f: impl FnOnce() + Send + 'static) {
    if stopping() {
        return;
    }
    let mut threads = THREADS.lock();
    threads.retain(|h| !h.is_finished());
    threads.push(std::thread::spawn(f));
}

pub fn stopping() -> bool {
    STOP.load(Ordering::Acquire)
}

pub fn shutdown() {
    STOP.store(true, Ordering::Release);
    let threads = std::mem::take(&mut *THREADS.lock());
    let deadline = Instant::now() + JOIN_LIMIT;
    for h in threads {
        while !h.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            tracing::warn!("BackupDiff_H: 履歴のスレッドが {} 秒で止まりませんでした", JOIN_LIMIT.as_secs());
        }
    }
}
