use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub struct ActiveGuard(Arc<AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn try_admit(active: &Arc<AtomicUsize>, limit: usize) -> Option<ActiveGuard> {
    let mut current = active.load(Ordering::SeqCst);
    loop {
        if current >= limit {
            return None;
        }
        match active.compare_exchange_weak(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return Some(ActiveGuard(Arc::clone(active))),
            Err(actual) => current = actual,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn admission_never_exceeds_limit() {
        const LIMIT: usize = 4;
        const WORKERS: usize = 16;
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(WORKERS));
        let mut handles = Vec::new();
        for _ in 0..WORKERS {
            let active = Arc::clone(&active);
            let peak = Arc::clone(&peak);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                for _ in 0..2000 {
                    if let Some(guard) = try_admit(&active, LIMIT) {
                        peak.fetch_max(active.load(Ordering::SeqCst), Ordering::SeqCst);
                        drop(guard);
                    }
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= LIMIT);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn admission_rejects_at_limit() {
        let active = Arc::new(AtomicUsize::new(0));
        let first = try_admit(&active, 1).unwrap();
        assert!(try_admit(&active, 1).is_none());
        drop(first);
        assert!(try_admit(&active, 1).is_some());
    }
}
