//! 抜き差しを待つ(PLAN.md 11.3)。
//!
//! コントローラが固まったメディアは、電源を入れ直す以外に復帰の手がない。
//! ソフトから USB の電源は切れないので、**人の手を電源スイッチとして使う**。
//! 「抜いて挿し直してください」と出して、実際に抜かれて戻ってくるのを待つ。
//!
//! 抜かれたことを先に確かめるのが要点。応答しないだけで OS からは見えたままの
//! ことが多く、それを「戻ってきた」と誤解すると、電源が入り直っていないまま
//! 読み直して同じところで固まる。
//!
//! 在否の確認にはデバイスノードの有無([`ofr_device::device_present`])を使う。
//! 列挙は macOS では `diskutil` を起こすので、固まった個体があると返ってこない
//! ことがある。列挙し直すのは、抜けたことを確かめてから(= 固まった個体が
//! いなくなってから)にする。

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ofr_device::{DeviceInfo, DeviceKind};

/// 在否を見に行く間隔。
const POLL: Duration = Duration::from_millis(500);

/// 列挙し直して ID の変化を見る間隔(ポーリング何回ごとか)。
const RESCAN_EVERY: u32 = 4;

/// 待っている間の状態。呼び出し側が画面に出す。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplugState {
    /// 抜かれるのを待っている。
    WaitingRemoval,
    /// 抜かれた。挿し直されるのを待っている。
    WaitingInsertion,
    /// 戻ってきた。中身は再開に使うデバイス ID(抜き差しで変わることがある)。
    Back(String),
    /// 待ったが戻ってこなかった。
    TimedOut,
    /// 中断された。
    Cancelled,
}

/// メディアが抜かれて挿し直されるまで待つ。
///
/// 戻ってきたら、そのデバイス ID を返す。時間切れ・中断・そもそも在否を
/// 判定できないデバイス(イメージファイルやテスト用モック)なら `None`。
///
/// `on_state` は状態が変わったときに呼ばれる。進捗表示のためのもので、
/// 同じ状態で何度も呼ぶことはない。
pub fn wait_for_replug(
    identity: &DeviceInfo,
    timeout: Duration,
    cancel: &AtomicBool,
    on_state: impl FnMut(&ReplugState),
) -> Option<String> {
    if !matches!(identity.kind, DeviceKind::PhysicalDisk | DeviceKind::Volume) {
        return None;
    }
    let identity_for_scan = identity.clone();
    wait_with(
        &identity.id,
        ofr_device::device_present,
        move || ofr_device::find_device_like(&identity_for_scan),
        timeout,
        POLL,
        cancel,
        on_state,
    )
}

/// [`wait_for_replug`] の中身。在否の確認手段を差し替えられるようにしてある。
fn wait_with(
    id: &str,
    present: impl Fn(&str) -> bool,
    rescan: impl Fn() -> Option<String>,
    timeout: Duration,
    poll: Duration,
    cancel: &AtomicBool,
    mut on_state: impl FnMut(&ReplugState),
) -> Option<String> {
    let deadline = Instant::now() + timeout;
    let mut polls = 0u32;

    // 第1段階: 抜かれるのを待つ。
    on_state(&ReplugState::WaitingRemoval);
    while present(id) {
        if cancel.load(Ordering::Relaxed) {
            on_state(&ReplugState::Cancelled);
            return None;
        }
        if Instant::now() >= deadline {
            on_state(&ReplugState::TimedOut);
            return None;
        }
        std::thread::sleep(poll);
    }

    // 第2段階: 挿し直されるのを待つ。ここまで来れば固まった個体はいないので、
    // ID が変わっていないかを列挙で見に行っても差し支えない。
    on_state(&ReplugState::WaitingInsertion);
    loop {
        if cancel.load(Ordering::Relaxed) {
            on_state(&ReplugState::Cancelled);
            return None;
        }
        if present(id) {
            let state = ReplugState::Back(id.to_string());
            on_state(&state);
            return Some(id.to_string());
        }
        polls += 1;
        if polls % RESCAN_EVERY == 0
            && let Some(other) = rescan()
        {
            let state = ReplugState::Back(other.clone());
            on_state(&state);
            return Some(other);
        }
        if Instant::now() >= deadline {
            on_state(&ReplugState::TimedOut);
            return None;
        }
        std::thread::sleep(poll);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// 何回目の呼び出しかで在否を変える確認関数。
    fn probe(present_until: u32, absent_until: u32) -> (impl Fn(&str) -> bool, Arc<AtomicU32>) {
        let calls = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&calls);
        let f = move |_: &str| {
            let n = counter.fetch_add(1, Ordering::Relaxed);
            n < present_until || n >= absent_until
        };
        (f, calls)
    }

    #[test]
    fn waits_for_removal_then_insertion() {
        // 2 回目までは見えている(まだ抜かれていない)、5 回目から戻ってくる。
        let (present, _) = probe(2, 5);
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();

        let back = wait_with(
            "disk4",
            present,
            || None,
            Duration::from_secs(5),
            Duration::from_millis(1),
            &cancel,
            |s| seen.push(s.clone()),
        );

        assert_eq!(back.as_deref(), Some("disk4"));
        assert_eq!(
            seen,
            vec![
                ReplugState::WaitingRemoval,
                ReplugState::WaitingInsertion,
                ReplugState::Back("disk4".to_string()),
            ]
        );
    }

    #[test]
    fn finds_the_device_again_under_a_new_id() {
        // 一度抜けたきり、元の ID では戻ってこない。列挙で見つけ直す。
        let (present, _) = probe(1, u32::MAX);
        let cancel = AtomicBool::new(false);

        let back = wait_with(
            "disk4",
            present,
            || Some("disk5".to_string()),
            Duration::from_secs(5),
            Duration::from_millis(1),
            &cancel,
            |_| {},
        );

        assert_eq!(
            back.as_deref(),
            Some("disk5"),
            "ID が変わった個体を拾えない"
        );
    }

    #[test]
    fn gives_up_when_nothing_happens() {
        // ずっと見えたまま = 抜かれていない。時間切れで諦める。
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let back = wait_with(
            "disk4",
            |_| true,
            || None,
            Duration::from_millis(1),
            Duration::from_millis(1),
            &cancel,
            |s| seen.push(s.clone()),
        );

        assert!(back.is_none());
        assert_eq!(seen.last(), Some(&ReplugState::TimedOut));
    }

    #[test]
    fn stops_when_cancelled() {
        let cancel = AtomicBool::new(true);
        let mut seen = Vec::new();
        let back = wait_with(
            "disk4",
            |_| true,
            || None,
            Duration::from_secs(60),
            Duration::from_millis(1),
            &cancel,
            |s| seen.push(s.clone()),
        );

        assert!(back.is_none());
        assert_eq!(seen.last(), Some(&ReplugState::Cancelled));
    }
}
