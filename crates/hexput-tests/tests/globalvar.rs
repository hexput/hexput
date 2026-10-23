//! Story 6.7: the per-Plugin Global Variable store (`hexput-globalvar`), driven directly.
//!
//! The store API these tests use follows the spec's Code Map: `GlobalVariables<V>` is the
//! registry (`create`, `get`, `teardown`), and each Plugin's `PluginStore<V>` reads and writes one
//! lock unit `(variable, top-level key)` — `key` is `None` for a whole variable (a scalar or an
//! array) and `Some(property)` for one top-level property of an object. `begin_update` takes the
//! unit's writer-intent lock and returns a guard that releases it on drop; `read` never takes it.
//! A whole variable's intent overlaps every property intent of that variable, since a whole write
//! rewrites every unit of it.
//!
//! The wire-level rows (persistence across fires, concurrent fires over sockets) live in
//! `daemon.rs`, where the Backend's stalled host call is driven; the Session's own teardown is
//! asserted here at the store only, because the registry is reached through the Daemon's wiring.

use std::sync::mpsc;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use hexput_globalvar::GlobalVariables;
use hexput_shared::ids::PluginId;

/// How long a thread the test expects to finish gets before the test calls it a hang.
const PATIENCE: Duration = Duration::from_secs(10);

/// A store for `id` holding `i64` values, created in `registry`.
fn store_for(registry: &GlobalVariables<i64>, id: u64) -> Arc<hexput_globalvar::PluginStore<i64>> {
    registry.create(PluginId::new(id))
}

#[test]
fn a_plugin_has_no_store_until_one_is_created_and_none_after_teardown() {
    let registry = GlobalVariables::<i64>::new();
    let id = PluginId::new(1);
    assert!(registry.get(id).is_none(), "nothing exists before creation");
    let _store = registry.create(id);
    assert!(registry.get(id).is_some(), "the created store is found");
    registry.teardown(id);
    assert!(
        registry.get(id).is_none(),
        "teardown removes the store: GlobalVariables has no entry for it"
    );
}

#[test]
fn teardown_removes_only_its_own_plugin_and_is_a_no_op_for_an_unknown_one() {
    let registry = GlobalVariables::<i64>::new();
    let (one, two) = (PluginId::new(1), PluginId::new(2));
    let kept = store_for(&registry, 2);
    kept.write("g", Some("a"), 7);
    let _ = store_for(&registry, 1);
    registry.teardown(one);
    registry.teardown(one);
    registry.teardown(PluginId::new(99));
    assert!(registry.get(one).is_none());
    let still = registry.get(two).expect("the other Plugin keeps its store");
    assert_eq!(still.read("g", Some("a")), Some(7));
}

#[test]
fn a_store_fetched_before_teardown_still_answers_its_holder_without_panicking() {
    // AD-4: teardown is the sole exit and never a drop; a run that already holds the store finishes
    // against it, while the registry no longer names it.
    let registry = GlobalVariables::<i64>::new();
    let id = PluginId::new(3);
    let held = registry.create(id);
    held.write("n", None, 4);
    registry.teardown(id);
    assert!(registry.get(id).is_none());
    assert_eq!(held.read("n", None), Some(4));
}

#[test]
fn a_fresh_store_holds_no_unit_so_a_read_finds_nothing() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    assert_eq!(store.read("g", Some("a")), None);
    assert_eq!(store.read("n", None), None);
}

#[test]
fn a_whole_variable_unit_and_a_property_unit_are_distinct_units() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    store.write("g", None, 1);
    store.write("g", Some("a"), 2);
    assert_eq!(store.read("g", None), Some(1));
    assert_eq!(store.read("g", Some("a")), Some(2));
    assert_eq!(store.read("g", Some("b")), None);
}

#[test]
fn a_write_replaces_the_value_and_the_last_writer_wins_whole() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    store.write("n", None, 1);
    store.write("n", None, 2);
    store.write("n", None, 3);
    assert_eq!(store.read("n", None), Some(3));
}

#[test]
fn a_read_never_waits_on_a_unit_whose_writer_intent_is_held() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    store.write("g", Some("a"), 5);
    let intent = store.begin_update("g", Some("a"));
    let reader = Arc::clone(&store);
    let (sent, received) = mpsc::channel();
    thread::spawn(move || {
        sent.send(reader.read("g", Some("a"))).unwrap();
    });
    let read = received
        .recv_timeout(PATIENCE)
        .expect("a read does not take the intent lock, so it cannot wait on it");
    assert_eq!(read, Some(5));
    drop(intent);
}

#[test]
fn a_second_writer_intent_on_the_same_unit_waits_until_the_first_is_released() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    let first = store.begin_update("g", Some("c"));
    let other = Arc::clone(&store);
    let (entered, taken) = mpsc::channel();
    thread::spawn(move || {
        let _second = other.begin_update("g", Some("c"));
        entered.send(()).unwrap();
    });
    assert!(
        taken.recv_timeout(Duration::from_millis(200)).is_err(),
        "the same unit's writer intent is exclusive"
    );
    drop(first);
    taken
        .recv_timeout(PATIENCE)
        .expect("the second intent proceeds once the first is released");
}

#[test]
fn writes_to_different_units_never_wait_on_each_other() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    // One unit is held for the whole test. Another thread's update of a different unit must still
    // finish: nothing shared between units blocks.
    let held = store.begin_update("g", Some("a"));
    let other = Arc::clone(&store);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let guard = other.begin_update("g", Some("b"));
        other.write("g", Some("b"), 9);
        drop(guard);
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(PATIENCE)
        .expect("a different unit proceeds while another unit's intent is held");
    drop(held);
    assert_eq!(store.read("g", Some("b")), Some(9));
}

/// Whether `begin_update(name, waiting)` on another thread returns while `held` is held: `true`
/// when it returned, `false` when it was still waiting after a short grace period (it is then
/// let through by dropping `held`, and must finish).
fn proceeds_while_held(
    store: &Arc<hexput_globalvar::PluginStore<i64>>,
    held: hexput_globalvar::Intent,
    name: &'static str,
    waiting: Option<&'static str>,
) -> bool {
    let other = Arc::clone(store);
    let (done, finished) = mpsc::channel();
    let thread = thread::spawn(move || {
        let guard = other.begin_update(name, waiting);
        drop(guard);
        done.send(()).unwrap();
    });
    let proceeded = finished.recv_timeout(Duration::from_millis(200)).is_ok();
    drop(held);
    if !proceeded {
        finished
            .recv_timeout(PATIENCE)
            .expect("the waiter proceeds once the intent is released");
    }
    thread.join().unwrap();
    proceeded
}

#[test]
fn a_whole_variable_intent_overlaps_every_property_intent_of_its_variable_and_no_other() {
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    // A whole write rewrites every unit of its variable, so it waits for a property's writer...
    let held = store.begin_update("g", Some("k"));
    assert!(!proceeds_while_held(&store, held, "g", None));
    // ...and a property's writer waits for a whole write.
    let held = store.begin_update("g", None);
    assert!(!proceeds_while_held(&store, held, "g", Some("k")));
    // Another variable is never in the way.
    let held = store.begin_update("g", None);
    assert!(proceeds_while_held(&store, held, "h", None));
    let held = store.begin_update("g", None);
    assert!(proceeds_while_held(&store, held, "h", Some("k")));
}

#[test]
fn an_update_under_its_intent_lock_loses_no_increment_across_threads() {
    // The safe strategy's guarantee at the store: a read-modify-write under the unit's intent lock
    // from many threads at once ends at the sum of every increment.
    let registry = GlobalVariables::<i64>::new();
    let store = store_for(&registry, 1);
    store.write("g", Some("c"), 0);
    let threads = 8;
    let barrier = Arc::new(Barrier::new(threads));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    let guard = store.begin_update("g", Some("c"));
                    let current = store.read("g", Some("c")).unwrap();
                    store.write("g", Some("c"), current + 1);
                    drop(guard);
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no thread panicked");
    }
    assert_eq!(store.read("g", Some("c")), Some(400));
}

#[test]
fn the_store_crate_has_no_async_and_depends_on_hexput_shared_and_dashmap_only() {
    // Acceptance: a reviewer greps `hexput-globalvar` for async and checks its dependencies.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../hexput-globalvar");
    let mut sources = Vec::new();
    collect_rs(std::path::Path::new(&format!("{root}/src")), &mut sources);
    assert!(!sources.is_empty(), "the store has sources");
    for source in sources {
        let text = std::fs::read_to_string(&source).unwrap();
        assert!(
            !text.contains(".await") && !text.contains("async fn") && !text.contains("async {"),
            "{} is synchronous",
            source.display()
        );
    }
    let manifest = std::fs::read_to_string(format!("{root}/Cargo.toml")).unwrap();
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .and_then(|rest| rest.split("\n[").next())
        .expect("a [dependencies] table");
    let names: Vec<&str> = dependencies
        .lines()
        .filter_map(|line| line.split('=').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    assert_eq!(names, ["hexput-shared", "dashmap"], "{dependencies}");
}

/// Every `.rs` file under `dir`, recursively.
fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
