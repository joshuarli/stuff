#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::publish::PublishError;

pub const REQUEST_CONCURRENCY: usize = 32;

pub fn map_bounded<T, R, F>(items: &[T], limit: usize, work: F) -> Result<Vec<R>, PublishError>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Result<R, PublishError> + Sync,
{
    let workers = limit.max(1).min(items.len());
    if workers <= 1 {
        return items.iter().map(work).collect();
    }
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let results: Mutex<Vec<Option<R>>> = Mutex::new(items.iter().map(|_| None).collect());
    let first_error: Mutex<Option<(usize, PublishError)>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                while !failed.load(Ordering::Acquire) {
                    let index = next.fetch_add(1, Ordering::AcqRel);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    match work(item) {
                        Ok(value) => {
                            results.lock().expect("result slots lock")[index] = Some(value);
                        }
                        Err(error) => {
                            failed.store(true, Ordering::Release);
                            let mut slot = first_error.lock().expect("error slot lock");
                            if slot.as_ref().is_none_or(|(held, _)| index < *held) {
                                *slot = Some((index, error));
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some((_, error)) = first_error.into_inner().expect("error slot lock") {
        return Err(error);
    }
    Ok(results
        .into_inner()
        .expect("result slots lock")
        .into_iter()
        .map(|slot| slot.expect("every item finished"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn results_keep_input_order() {
        let items: Vec<usize> = (0..50).collect();
        let doubled = map_bounded(&items, 8, |item| Ok(item * 2)).expect("no failures");
        assert_eq!(doubled, (0..50).map(|item| item * 2).collect::<Vec<_>>());
    }

    #[test]
    fn runs_every_item_with_bounded_workers() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let items: Vec<usize> = (0..64).collect();
        map_bounded(&items, 4, |_| {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(2));
            running.fetch_sub(1, Ordering::SeqCst);
            done.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .expect("all items succeed");
        assert_eq!(done.load(Ordering::SeqCst), 64);
        assert!(peak.load(Ordering::SeqCst) <= 4);
        assert!(peak.load(Ordering::SeqCst) > 1);
    }

    #[test]
    fn the_first_error_stops_new_work_and_is_returned() {
        let started = AtomicUsize::new(0);
        let items: Vec<usize> = (0..1000).collect();
        let error = map_bounded(&items, 4, |item| {
            started.fetch_add(1, Ordering::SeqCst);
            if *item == 3 {
                return Err(PublishError::Transport("boom".to_string()));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            Ok(())
        })
        .expect_err("item 3 fails");
        assert!(matches!(error, PublishError::Transport(ref text) if text == "boom"));
        assert!(started.load(Ordering::SeqCst) < 100);
    }
}
