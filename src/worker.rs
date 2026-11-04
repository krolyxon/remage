//! Priority job pool with epoch cancellation.
//!
//! Jobs are keyed: submitting a key that is already queued replaces the queued job (new
//! priority, new epoch), and submitting one that is running is a no-op. Bumping the epoch makes
//! every queued job of an epoch-sensitive priority stale, so mashing `n` never decodes the
//! images that were skipped over.

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// The image on screen.
    Current = 0,
    /// Neighbours of the current image.
    Preload = 1,
    /// Thumbnails in view.
    ThumbVisible = 2,
    /// Other thumbnails.
    Thumb = 3,
}

impl Priority {
    fn epoch_sensitive(self) -> bool {
        matches!(self, Priority::Current | Priority::Preload)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum JobKey {
    Decode(PathBuf),
    Thumb(PathBuf),
}

type Work = Box<dyn FnOnce() + Send>;

struct Entry {
    prio: Priority,
    seq: u64,
    epoch: u64,
    key: JobKey,
    work: Work,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.prio == other.prio && self.seq == other.seq
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    /// Max-heap order: lowest priority value first, then oldest submission.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other
            .prio
            .cmp(&self.prio)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

#[derive(Default)]
struct Queue {
    heap: BinaryHeap<Entry>,
    /// Key → sequence number of its live (non-superseded) entry.
    live: HashMap<JobKey, u64>,
    running: HashSet<JobKey>,
    seq: u64,
    epoch: u64,
    shutdown: bool,
}

impl Queue {
    fn pop(&mut self) -> Option<Entry> {
        while let Some(e) = self.heap.pop() {
            if self.live.get(&e.key) != Some(&e.seq) {
                continue; // superseded by a later submit
            }
            self.live.remove(&e.key);
            if e.prio.epoch_sensitive() && e.epoch < self.epoch {
                continue; // stale
            }
            self.running.insert(e.key.clone());
            return Some(e);
        }
        None
    }
}

struct Shared {
    queue: Mutex<Queue>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        // A panicking job never holds the lock, so poisoning can't leave it inconsistent.
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Pool {
    shared: Arc<Shared>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl Pool {
    /// Start `nproc - 1` workers (at least one).
    pub fn new() -> Self {
        let n = thread::available_parallelism()
            .map_or(2, |n| n.get())
            .saturating_sub(1)
            .max(1);
        Self::with_threads(n)
    }

    pub fn with_threads(n: usize) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            cv: Condvar::new(),
        });
        let threads = (0..n)
            .map(|i| {
                let shared = shared.clone();
                thread::Builder::new()
                    .name(format!("remage-worker-{i}"))
                    .spawn(move || worker(&shared))
                    .expect("spawn worker thread")
            })
            .collect();
        Self { shared, threads }
    }

    /// Queue `work` under `key` unless that key is already running.
    pub fn submit(&self, key: JobKey, prio: Priority, work: impl FnOnce() + Send + 'static) {
        let mut q = self.shared.lock();
        if q.running.contains(&key) {
            return;
        }
        q.seq += 1;
        let (seq, epoch) = (q.seq, q.epoch);
        q.live.insert(key.clone(), seq);
        q.heap.push(Entry {
            prio,
            seq,
            epoch,
            key,
            work: Box::new(work),
        });
        drop(q);
        self.shared.cv.notify_one();
    }

    /// Drop `key` if it is still queued (a running job finishes regardless).
    pub fn cancel(&self, key: &JobKey) {
        self.shared.lock().live.remove(key);
    }

    /// Invalidate every queued current/preload job.
    pub fn bump_epoch(&self) {
        self.shared.lock().epoch += 1;
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.shared.cv.notify_all();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

fn worker(shared: &Shared) {
    loop {
        let entry = {
            let mut q = shared.lock();
            loop {
                if q.shutdown {
                    return;
                }
                if let Some(e) = q.pop() {
                    break e;
                }
                q = shared.cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let key = entry.key.clone();
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(entry.work)).is_err() {
            log::error!("worker job {key:?} panicked");
        }
        shared.lock().running.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::time::Duration;

    fn key(s: &str) -> JobKey {
        JobKey::Decode(PathBuf::from(s))
    }

    #[test]
    fn runs_by_priority_and_drops_stale_jobs() {
        let pool = Pool::with_threads(1);
        let (gate_tx, gate_rx) = unbounded::<()>();
        let (tx, rx) = unbounded();
        // Block the single worker so the rest queue up.
        pool.submit(key("gate"), Priority::Current, move || {
            let _ = gate_rx.recv();
        });
        std::thread::sleep(Duration::from_millis(50));
        for (k, p) in [("thumb", Priority::Thumb), ("stale", Priority::Preload)] {
            let tx = tx.clone();
            pool.submit(key(k), p, move || tx.send(k).unwrap());
        }
        pool.bump_epoch();
        for (k, p) in [("pre", Priority::Preload), ("cur", Priority::Current)] {
            let tx = tx.clone();
            pool.submit(key(k), p, move || tx.send(k).unwrap());
        }
        gate_tx.send(()).unwrap();
        let got: Vec<_> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        assert_eq!(got, ["cur", "pre", "thumb"]);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn resubmitting_replaces_the_queued_job() {
        let pool = Pool::with_threads(1);
        let (gate_tx, gate_rx) = unbounded::<()>();
        let (tx, rx) = unbounded();
        pool.submit(key("gate"), Priority::Current, move || {
            let _ = gate_rx.recv();
        });
        std::thread::sleep(Duration::from_millis(50));
        for tag in ["old", "new"] {
            let tx = tx.clone();
            pool.submit(key("a"), Priority::Preload, move || tx.send(tag).unwrap());
        }
        gate_tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "new");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn cancelled_jobs_never_run() {
        let pool = Pool::with_threads(1);
        let (gate_tx, gate_rx) = unbounded::<()>();
        let (tx, rx) = unbounded();
        pool.submit(key("gate"), Priority::Current, move || {
            let _ = gate_rx.recv();
        });
        std::thread::sleep(Duration::from_millis(50));
        for k in ["a", "b"] {
            let tx = tx.clone();
            let job = JobKey::Thumb(PathBuf::from(k));
            pool.submit(job, Priority::Thumb, move || tx.send(k).unwrap());
        }
        pool.cancel(&JobKey::Thumb(PathBuf::from("a")));
        gate_tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "b");
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    }
}
