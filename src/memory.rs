//! Memory of large results, given back to the system once they are replaced.
//!
//! glibc keeps the pages it frees for later allocations: after a page of large
//! documents (a few hundred MB as `Bson` values), the process would stay as
//! big as at its peak whatever comes next. `malloc_trim` gives them back.

use std::sync::OnceLock;

use crossbeam_channel::{Sender, unbounded};

type Garbage = Box<dyn Send>;

/// Drops `garbage` on a background thread, then gives the memory freed back
/// to the system. Dropping hundreds of MB of documents takes tens of
/// milliseconds, and so does trimming the heap: not on the UI thread.
pub fn release<T: Send + 'static>(garbage: T) {
    static TX: OnceLock<Option<Sender<Garbage>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = unbounded::<Garbage>();
        std::thread::Builder::new()
            .name("memory".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    drop(first);
                    // Several results at once (a disconnection): one trim.
                    while let Ok(more) = rx.try_recv() {
                        drop(more);
                    }
                    trim();
                }
            })
            .ok()
            .map(|_| tx)
    });
    match tx {
        Some(tx) => {
            if let Err(e) = tx.send(Box::new(garbage)) {
                drop(e.into_inner());
            }
        }
        None => drop(garbage),
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    // SAFETY: no precondition; glibc locks each arena while trimming it.
    unsafe { malloc_trim(0) };
}

/// The other allocators give large freed blocks back by themselves.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Probe(Sender<std::thread::ThreadId>);

    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.0.send(std::thread::current().id());
        }
    }

    #[test]
    fn released_values_are_dropped_on_another_thread() {
        let (tx, rx) = unbounded();
        release(Probe(tx.clone()));
        release(vec![Probe(tx.clone()), Probe(tx)]);
        let me = std::thread::current().id();
        for _ in 0..3 {
            let by = rx.recv_timeout(Duration::from_secs(5)).expect("dropped");
            assert_ne!(by, me);
        }
    }
}
