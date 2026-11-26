//! The event pump against GPUI's real event loop (the Linux platform with no windowing):
//! 10,000 events from 8 threads cost the UI thread one wake and one turn of the pump per
//! burst, however many threads and events the burst has.
//!
//! One test in this binary: it owns the process's GPUI application.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use decibell_core::events::{CoreEvent, VoicePingUpdatedPayload};
use decibell_model::pump::{EventQueue, Incoming, spawn_pump};
use futures::StreamExt as _;

const THREADS: u32 = 8;
const BURSTS: u32 = 5;
const PER_THREAD_PER_BURST: u32 = 250; // 8 × 250 × 5 = 10,000

fn emit_burst(queue: &Arc<EventQueue>, burst: u32) {
    let threads: Vec<_> = (0..THREADS)
        .map(|t| {
            let queue = queue.clone();
            std::thread::Builder::new()
                .name(format!("pump-test-{t}"))
                .spawn(move || {
                    for i in 0..PER_THREAD_PER_BURST {
                        let n = burst * 1_000_000 + t * 10_000 + i;
                        queue.push(Incoming::Core(CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload {
                            latency_ms: n,
                        })));
                    }
                })
                .unwrap()
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn ten_thousand_events_from_eight_threads_cost_one_wake_per_burst() {
    type AfterEachBurst = Vec<(u64, u64, u64)>;
    let outcome: Arc<std::sync::Mutex<Option<AfterEachBurst>>> = Arc::default();
    let seen_total = Arc::new(std::sync::atomic::AtomicU64::new(0));
    gpui::Application::with_platform(gpui_platform::current_platform(true)).run({
        let (outcome, seen_total) = (outcome.clone(), seen_total.clone());
        move |cx| {
            let (queue, woken) = EventQueue::new();
            let seen: Rc<RefCell<Vec<u32>>> = Rc::default();
            let updates = Rc::new(RefCell::new(0u64));
            let (applied_tx, mut applied_rx) = futures::channel::mpsc::unbounded::<usize>();
            let pump = spawn_pump(queue.clone(), woken, cx, {
                let (seen, updates) = (seen.clone(), updates.clone());
                move |batch, _| {
                    *updates.borrow_mut() += 1;
                    for item in batch.drain(..) {
                        if let Incoming::Core(CoreEvent::VoicePingUpdated(p)) = item {
                            seen.borrow_mut().push(p.latency_ms);
                        }
                    }
                    let _ = applied_tx.unbounded_send(seen.borrow().len());
                }
            });
            cx.spawn(async move |cx| {
                let _pump = pump;
                let mut after_each_burst = Vec::new();
                for burst in 0..BURSTS {
                    // The UI thread is busy (here: joining the producers) while the
                    // whole burst arrives from 8 threads.
                    emit_burst(&queue, burst);
                    // Back to the event loop: wait until the pump has applied it.
                    let target = ((burst + 1) * THREADS * PER_THREAD_PER_BURST) as usize;
                    while applied_rx.next().await.is_some_and(|applied| applied < target) {}
                    let stats = queue.stats();
                    after_each_burst.push((stats.events, stats.wakes, stats.drains));
                    assert_eq!(*updates.borrow(), stats.drains, "one App::update per turn");
                }
                // Per producer thread and burst, order is kept.
                let seen = seen.borrow();
                for burst in 0..BURSTS {
                    for t in 0..THREADS {
                        let of_thread: Vec<u32> = seen
                            .iter()
                            .copied()
                            .filter(|v| v / 1_000_000 == burst && (v % 1_000_000) / 10_000 == t)
                            .collect();
                        assert_eq!(of_thread.len() as u32, PER_THREAD_PER_BURST);
                        assert!(of_thread.windows(2).all(|w| w[0] < w[1]));
                    }
                }
                seen_total.store(seen.len() as u64, std::sync::atomic::Ordering::Relaxed);
                *outcome.lock().unwrap() = Some(after_each_burst);
                cx.update(|cx| cx.quit());
            })
            .detach();
        }
    });
    decibell_model::core::shutdown(std::time::Duration::from_secs(1));
    let after_each_burst = outcome.lock().unwrap().take().expect("the scenario ran to its end");
    println!("(events, wakes, drains) after each burst: {after_each_burst:?}");
    assert_eq!(seen_total.load(std::sync::atomic::Ordering::Relaxed), 10_000);
    let expected: Vec<(u64, u64, u64)> = (1..=BURSTS as u64).map(|n| (n * 2000, n, n)).collect();
    assert_eq!(after_each_burst, expected, "one wake and one turn per burst of 2,000 events from 8 threads");
}
