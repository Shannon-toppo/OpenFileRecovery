//! 応答が返らない読み込みを見捨てるためのラッパー(PLAN.md 11.2)。
//!
//! 壊れかけた USB メモリや SD カードは、コントローラが固まると読み込みが
//! **返ってこなくなる**。要求を出したスレッドは OS の中で割り込み不可の待ちに入り、
//! そこから戻らない。キャンセルフラグは読み込みと読み込みの間でしか見ないので、
//! この状態になると吸い出し全体が止まる(実機で 9 時間戻らなかった例がある)。
//!
//! 戻らないスレッドを外から回収する方法はない。そこで、
//!
//! 1. 実際の読み込みは専用スレッド(ワーカー)にやらせる
//! 2. 制限時間内に返らなければ、**そのスレッドごと見捨てる**
//! 3. デバイスを開き直して、新しいワーカーで先へ進む
//!
//! という作りにしてある。見捨てたスレッドはチャネルを落として通知するので、
//! 読み込みがいつか返れば送信に失敗して自分で終わる。返らなければプロセスの
//! 終了まで残るが、[`TimeoutDevice`] は次のデバイスを開いて動き続けられる。
//!
//! 見捨てたスレッドはデバイスハンドルとバッファを握ったままになるので、
//! 際限なく増やすわけにはいかない。上限を超えたら
//! [`DeviceError::Unresponsive`] を返して「メディアを抜き差ししてほしい」と
//! 上位へ伝える。

use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use tracing::warn;

use crate::device::{Device, DeviceInfo, clamp_read};
use crate::error::{DeviceError, Result};

/// デバイスを開き直すための生成関数。
///
/// 固まったハンドルは捨てるしかないので、開き直しは「開く操作をもう一度やる」
/// 形でしか実装できない。
pub type DeviceFactory = Box<dyn Fn() -> Result<Box<dyn Device>> + Send + Sync>;

/// 読み込みを見限るまでの既定の待ち時間。
///
/// 生きているが遅いだけのメディアを切り捨てないよう、SCSI のコマンド
/// タイムアウトと同じくらいに取ってある。
pub const DEFAULT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// 見捨てたワーカーの既定の上限。
///
/// 1 本につきデバイスハンドルとバッファが道連れになるので、際限なくは増やせない。
/// これだけ固まるメディアは電源を入れ直す(抜き差しする)以外に手がない。
pub const DEFAULT_MAX_ABANDONED: u32 = 8;

/// ワーカーへの読み込み依頼。バッファごと渡す。
///
/// 見捨てたスレッドが呼び出し側のバッファに書き込むことがあってはならないので、
/// バッファの所有権をワーカーへ移し、結果と一緒に返してもらう。
struct Request {
    offset: u64,
    len: usize,
    buf: Vec<u8>,
}

/// ワーカーからの応答。
struct Response {
    result: Result<usize>,
    buf: Vec<u8>,
}

/// 実デバイスを握っている IO スレッド。
struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Response>,
    /// 使い回すバッファ。見捨てたワーカーと一緒に失われる。
    buf: Vec<u8>,
}

impl Worker {
    /// デバイスを預けて IO スレッドを起こす。
    fn spawn(device: Box<dyn Device>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<Request>();
        let (res_tx, res_rx) = mpsc::channel::<Response>();

        // join しない。固まったスレッドは join できないので、見捨てられる形にしておく。
        let spawned = std::thread::Builder::new()
            .name("ofr-io".to_string())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    let result = device.read_at(req.offset, &mut req.buf[..req.len]);
                    // 送信に失敗する = 見捨てられた。デバイスを落として終わる。
                    if res_tx
                        .send(Response {
                            result,
                            buf: req.buf,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            });
        if let Err(e) = &spawned {
            warn!(error = %e, "IO スレッドを起こせなかった");
        }

        Self {
            tx: req_tx,
            rx: res_rx,
            buf: Vec::new(),
        }
    }
}

/// 読み込みに制限時間を設ける [`Device`] ラッパー。
///
/// 制限時間を過ぎた読み込みは [`DeviceError::Timeout`] になる。上位の
/// イメージングエンジンから見れば「読めなかった領域」なので、記録して先へ進める。
pub struct TimeoutDevice {
    factory: DeviceFactory,
    timeout: Duration,
    max_abandoned: u32,
    worker: Mutex<Option<Worker>>,
    abandoned: AtomicU32,
    len: u64,
    info: DeviceInfo,
}

impl std::fmt::Debug for TimeoutDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TimeoutDevice")
            .field("id", &self.info.id)
            .field("timeout", &self.timeout)
            .field("abandoned", &self.abandoned())
            .finish()
    }
}

impl TimeoutDevice {
    /// デバイスを開いて包む。
    ///
    /// `factory` は開き直しにも使うので、何度呼んでも同じデバイスを開けること。
    /// `timeout` が 0 なら制限時間なし(ワーカー越しに読むだけ)になる。
    pub fn new(factory: DeviceFactory, timeout: Duration, max_abandoned: u32) -> Result<Self> {
        let device = factory()?;
        let len = device.len();
        let info = device.info().clone();
        Ok(Self {
            factory,
            timeout,
            max_abandoned,
            worker: Mutex::new(Some(Worker::spawn(device))),
            abandoned: AtomicU32::new(0),
            len,
            info,
        })
    }

    /// 見捨てたワーカーの本数。
    pub fn abandoned(&self) -> u32 {
        self.abandoned.load(Ordering::Relaxed)
    }

    /// 制限時間。
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// 1 回の読み込みの結末。ワーカーの借用を切ってから後始末するために挟む。
enum Outcome {
    Done(Result<usize>),
    TimedOut,
    Dead,
}

impl Device for TimeoutDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let Some(want) = clamp_read(offset, buf.len(), self.len) else {
            return Ok(0);
        };

        let mut guard = self.worker.lock().expect("TimeoutDevice poisoned");
        if guard.is_none() {
            // 前の読み込みで見捨てたあと。ここで開き直す。
            *guard = Some(Worker::spawn((self.factory)()?));
        }

        let outcome = {
            let worker = guard.as_mut().expect("worker");
            let mut work = std::mem::take(&mut worker.buf);
            if work.len() < want {
                work.resize(want, 0);
            }
            let sent = worker.tx.send(Request {
                offset,
                len: want,
                buf: work,
            });
            if sent.is_err() {
                Outcome::Dead
            } else {
                let waited = if self.timeout.is_zero() {
                    worker.rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
                } else {
                    worker.rx.recv_timeout(self.timeout)
                };
                match waited {
                    Ok(res) => {
                        if let Ok(n) = &res.result {
                            buf[..*n].copy_from_slice(&res.buf[..*n]);
                        }
                        worker.buf = res.buf;
                        Outcome::Done(res.result)
                    }
                    Err(RecvTimeoutError::Timeout) => Outcome::TimedOut,
                    Err(RecvTimeoutError::Disconnected) => Outcome::Dead,
                }
            }
        };

        match outcome {
            Outcome::Done(result) => result,
            Outcome::TimedOut => {
                // このワーカーはもう戻らない。チャネルごと捨てる。
                *guard = None;
                let n = self.abandoned.fetch_add(1, Ordering::Relaxed) + 1;
                warn!(
                    offset,
                    len = want,
                    timeout_secs = self.timeout.as_secs_f64(),
                    abandoned = n,
                    "読み込みが返らないので IO スレッドを見捨てる"
                );
                if n >= self.max_abandoned {
                    return Err(DeviceError::Unresponsive { abandoned: n });
                }
                Err(DeviceError::Timeout {
                    offset,
                    len: want,
                    waited: self.timeout,
                })
            }
            Outcome::Dead => {
                *guard = None;
                Err(DeviceError::Io {
                    offset,
                    len: want,
                    source: io::Error::other("IO スレッドが停止した"),
                })
            }
        }
    }

    fn len(&self) -> u64 {
        self.len
    }

    fn block_size(&self) -> u32 {
        self.info.block_size
    }

    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn reopen(&self) -> Result<bool> {
        let mut guard = self.worker.lock().expect("TimeoutDevice poisoned");
        // 今のワーカーは畳む。固着していれば、読み込みが返り次第そのスレッドは終わる。
        *guard = None;
        *guard = Some(Worker::spawn((self.factory)()?));
        Ok(true)
    }

    fn recovered(&self) -> Result<bool> {
        // 抜き差しで電源が入り直った。見捨てた本数の計数からやり直す(PLAN.md 11.3)。
        self.abandoned.store(0, Ordering::Relaxed);
        self.reopen()
    }
}
