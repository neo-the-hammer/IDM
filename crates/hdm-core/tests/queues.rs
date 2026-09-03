//! Queues, schedules and completion actions.

use hdm_core::engine::{DownloadSpec, Status};
use hdm_core::manager::Manager;
use hdm_core::platform::LocalTime;
use hdm_core::queue::{Completion, Queue, Schedule, EVERY_DAY, MAIN_QUEUE, WEEKDAYS, WEEKENDS};
use hdm_core::store::Store;
use hdm_json::Json;
use hdm_testserver::tls::TempDir;
use hdm_testserver::{test_data, ServerBuilder};
use std::path::PathBuf;

fn at(hour: u8, minute: u8, weekday: u8) -> LocalTime {
    LocalTime {
        hour,
        minute,
        weekday,
    }
}

// ------------------------------------------------------------------ schedule

#[test]
fn a_disabled_schedule_is_always_open() {
    let schedule = Schedule::default();
    assert!(!schedule.enabled);
    assert!(schedule.is_open(at(3, 0, 1)));
    assert!(schedule.is_open(at(15, 30, 5)));
}

#[test]
fn a_daytime_window_opens_and_closes() {
    // 09:00 to 17:00, every day.
    let schedule = Schedule {
        enabled: true,
        start: 9 * 60,
        stop: Some(17 * 60),
        days: EVERY_DAY,
    };
    assert!(!schedule.is_open(at(8, 59, 3)));
    assert!(schedule.is_open(at(9, 0, 3)));
    assert!(schedule.is_open(at(12, 0, 3)));
    assert!(schedule.is_open(at(16, 59, 3)));
    assert!(
        !schedule.is_open(at(17, 0, 3)),
        "the stop time is exclusive"
    );
    assert!(!schedule.is_open(at(23, 0, 3)));
}

/// "23:00 to 06:00" is the obvious way to write an overnight queue, and
/// refusing it as an inverted range would be a poor reading of the intent.
#[test]
fn an_overnight_window_wraps_past_midnight() {
    let schedule = Schedule {
        enabled: true,
        start: 23 * 60,
        stop: Some(6 * 60),
        days: EVERY_DAY,
    };
    assert!(schedule.is_open(at(23, 30, 2)), "late evening");
    assert!(schedule.is_open(at(0, 30, 3)), "after midnight");
    assert!(schedule.is_open(at(5, 59, 3)), "just before the close");
    assert!(!schedule.is_open(at(6, 0, 3)), "after the close");
    assert!(!schedule.is_open(at(12, 0, 3)), "the middle of the day");
    assert!(!schedule.is_open(at(22, 59, 2)), "just before it opens");
}

#[test]
fn a_window_with_no_stop_runs_until_it_is_empty() {
    let schedule = Schedule {
        enabled: true,
        start: 2 * 60,
        stop: None,
        days: EVERY_DAY,
    };
    assert!(!schedule.is_open(at(1, 59, 4)));
    assert!(schedule.is_open(at(2, 0, 4)));
    assert!(schedule.is_open(at(23, 59, 4)));
}

#[test]
fn day_masks_select_the_right_days() {
    // Weekdays only, 09:00 onwards. Bit 0 is Sunday.
    let weekdays = Schedule {
        enabled: true,
        start: 9 * 60,
        stop: None,
        days: WEEKDAYS,
    };
    assert!(!weekdays.is_open(at(10, 0, 0)), "Sunday");
    assert!(weekdays.is_open(at(10, 0, 1)), "Monday");
    assert!(weekdays.is_open(at(10, 0, 5)), "Friday");
    assert!(!weekdays.is_open(at(10, 0, 6)), "Saturday");

    let weekends = Schedule {
        enabled: true,
        start: 0,
        stop: None,
        days: WEEKENDS,
    };
    assert!(weekends.is_open(at(10, 0, 0)), "Sunday");
    assert!(weekends.is_open(at(10, 0, 6)), "Saturday");
    assert!(!weekends.is_open(at(10, 0, 3)), "Wednesday");
}

/// An overnight window that started on an allowed day must stay open into the
/// small hours of the next one, even if that day is not itself selected.
#[test]
fn an_overnight_window_carries_into_an_unselected_day() {
    // Friday nights only: 22:00 to 04:00. Bit 5 is Friday.
    let schedule = Schedule {
        enabled: true,
        start: 22 * 60,
        stop: Some(4 * 60),
        days: 1 << 5,
    };
    assert!(schedule.is_open(at(23, 0, 5)), "Friday evening");
    assert!(schedule.is_open(at(2, 0, 6)), "the small hours of Saturday");
    assert!(!schedule.is_open(at(12, 0, 6)), "Saturday afternoon");
    assert!(!schedule.is_open(at(23, 0, 2)), "Tuesday evening");
}

#[test]
fn schedules_survive_a_json_round_trip() {
    let schedule = Schedule {
        enabled: true,
        start: 1350,
        stop: Some(390),
        days: WEEKDAYS,
    };
    assert_eq!(Schedule::from_json(&schedule.to_json()), schedule);
}

// -------------------------------------------------------------------- queues

#[test]
fn a_paused_queue_starts_nothing() {
    let mut queue = Queue::new("q", "Q");
    assert!(queue.is_runnable(at(12, 0, 3)));
    queue.paused = true;
    assert!(
        !queue.is_runnable(at(12, 0, 3)),
        "a paused queue must stay idle"
    );
}

#[test]
fn queues_survive_a_json_round_trip() {
    let mut queue = Queue::new("overnight", "Overnight");
    queue.concurrency = 12;
    queue.speed_limit = 1024 * 900;
    queue.completion = Completion::Shutdown;
    queue.schedule = Schedule {
        enabled: true,
        start: 60,
        stop: Some(420),
        days: EVERY_DAY,
    };
    assert_eq!(Queue::from_json(&queue.to_json()).unwrap(), queue);
}

#[test]
fn completion_actions_round_trip() {
    for action in [
        Completion::Nothing,
        Completion::Shutdown,
        Completion::Sleep,
        Completion::Hibernate,
        Completion::Exit,
        Completion::Run("/usr/bin/echo done".into()),
    ] {
        assert_eq!(Completion::from_json(&action.to_json()), action);
    }
}

#[test]
fn default_queues_include_a_main_and_an_overnight_one() {
    let store_dir = TempDir::new("hydra-defaultq").unwrap();
    let store = Store::load(
        &store_dir.path().join("s.json"),
        store_dir.path().to_path_buf(),
    );
    let ids: Vec<&str> = store
        .settings
        .queues
        .iter()
        .map(|q| q.id.as_str())
        .collect();
    assert!(ids.contains(&MAIN_QUEUE));
    assert!(ids.contains(&"overnight"));

    let overnight = store
        .settings
        .queues
        .iter()
        .find(|q| q.id == "overnight")
        .unwrap();
    assert!(
        overnight.schedule.enabled,
        "the overnight queue should be scheduled"
    );
}

/// A download pointing at a queue that no longer exists must still run.
#[test]
fn a_missing_queue_falls_back_to_main() {
    let dir = TempDir::new("hydra-fallbackq").unwrap();
    let store = Store::load(&dir.path().join("s.json"), dir.path().to_path_buf());
    assert_eq!(
        store.settings.queue_for(Some("does-not-exist")).id,
        MAIN_QUEUE
    );
    assert_eq!(store.settings.queue_for(None).id, MAIN_QUEUE);
}

// ------------------------------------------------------------- the manager

fn manager() -> (Arc, TempDir) {
    let dir = TempDir::new("hydra-mgr-queue").unwrap();
    let manager = Manager::load(dir.path(), dir.path().to_path_buf());
    (manager, dir)
}
type Arc = std::sync::Arc<Manager>;

#[test]
fn queues_can_be_created_updated_and_removed() {
    let (manager, _dir) = manager();
    let mut queue = Queue::new("mirrors", "Mirrors");
    queue.concurrency = 3;
    manager.put_queue(queue.clone()).unwrap();
    assert!(manager.queues().iter().any(|q| q.id == "mirrors"));

    queue.concurrency = 6;
    manager.put_queue(queue).unwrap();
    let stored = manager
        .queues()
        .into_iter()
        .find(|q| q.id == "mirrors")
        .unwrap();
    assert_eq!(
        stored.concurrency, 6,
        "putting an existing id must update it"
    );

    manager.remove_queue("mirrors").unwrap();
    assert!(!manager.queues().iter().any(|q| q.id == "mirrors"));
}

/// Removing the main queue would leave downloads with nowhere to run.
#[test]
fn the_main_queue_cannot_be_removed() {
    let (manager, _dir) = manager();
    assert!(manager.remove_queue(MAIN_QUEUE).is_err());
}

/// Deleting a queue must not strand the downloads that were in it.
#[test]
fn removing_a_queue_moves_its_downloads_to_main() {
    let (manager, dir) = manager();
    manager.put_queue(Queue::new("temp", "Temp")).unwrap();
    let id = manager.add(
        DownloadSpec::new("https://example.com/f.bin", dir.path().to_path_buf()),
        None,
        false,
    );
    manager.set_queue(&id, Some("temp")).unwrap();
    assert_eq!(
        manager.snapshot_one(&id).unwrap().str_or("queue", ""),
        "temp"
    );

    manager.remove_queue("temp").unwrap();
    assert_eq!(
        manager.snapshot_one(&id).unwrap().str_or("queue", ""),
        MAIN_QUEUE,
        "the download was left pointing at a deleted queue"
    );
}

#[test]
fn moving_a_download_to_an_unknown_queue_is_refused() {
    let (manager, dir) = manager();
    let id = manager.add(
        DownloadSpec::new("https://example.com/f.bin", dir.path().to_path_buf()),
        None,
        false,
    );
    assert!(manager.set_queue(&id, Some("nope")).is_err());
}

/// A queue outside its window must hold its downloads back, even though they
/// are queued and the global concurrency limit has room.
#[test]
fn a_closed_schedule_holds_downloads_back() {
    let (manager, dir) = manager();
    let server = ServerBuilder::new().file("/f.bin", test_data(1024)).start();

    // A window that is open for one minute a day, in the past or future but
    // never now: whichever minute it is, this is closed.
    let now = hdm_core::platform::local_time().minutes();
    let closed_start = (now + 120) % 1440;
    let mut queue = Queue::new("later", "Later");
    queue.schedule = Schedule {
        enabled: true,
        start: closed_start,
        stop: Some((closed_start + 1) % 1440),
        days: EVERY_DAY,
    };
    manager.put_queue(queue).unwrap();

    let id = manager.add(
        DownloadSpec::new(&server.url("/f.bin"), dir.path().to_path_buf()),
        None,
        true,
    );
    manager.set_queue(&id, Some("later")).unwrap();

    // Several ticks: nothing should start.
    for _ in 0..5 {
        manager.tick();
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let snapshot = manager.snapshot_one(&id).unwrap();
    assert_eq!(
        snapshot.str_or("status", ""),
        Status::Queued.as_str(),
        "a download in a closed window must not start"
    );
}

/// The same download in an open window must start, which is what shows the
/// previous test is measuring the schedule and not something else.
#[test]
fn an_open_schedule_lets_downloads_run() {
    let (manager, dir) = manager();
    let server = ServerBuilder::new().file("/f.bin", test_data(4096)).start();

    let mut queue = Queue::new("now", "Now");
    queue.schedule = Schedule {
        enabled: true,
        start: 0,
        stop: None,
        days: EVERY_DAY,
    };
    manager.put_queue(queue).unwrap();

    let id = manager.add(
        DownloadSpec::new(&server.url("/f.bin"), dir.path().to_path_buf()),
        None,
        true,
    );
    manager.set_queue(&id, Some("now")).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        manager.tick();
        let status = manager
            .snapshot_one(&id)
            .unwrap()
            .str_or("status", "")
            .to_string();
        if status == "completed" || std::time::Instant::now() > deadline {
            assert_eq!(status, "completed", "an open window should let it run");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Pausing a queue must stop what is already running in it, not merely stop
/// new work from starting.
#[test]
fn pausing_a_queue_stops_its_downloads() {
    let (manager, dir) = manager();
    let server = ServerBuilder::new()
        .file_with("/big.bin", test_data(4_000_000), |f| {
            f.delay_per_chunk = Some(std::time::Duration::from_millis(15));
        })
        .start();

    let id = manager.add(
        DownloadSpec::new(&server.url("/big.bin"), dir.path().to_path_buf()),
        None,
        true,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        manager.tick();
        if manager.snapshot_one(&id).unwrap().str_or("status", "") == "downloading" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the download never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    manager.set_queue_paused(MAIN_QUEUE, true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        manager.tick();
        let status = manager
            .snapshot_one(&id)
            .unwrap()
            .str_or("status", "")
            .to_string();
        if status != "downloading" {
            assert_eq!(status, "paused");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pausing the queue had no effect"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Per-queue concurrency must cap that queue without capping the daemon.
#[test]
fn queue_concurrency_is_enforced() {
    let (manager, dir) = manager();
    let server = ServerBuilder::new()
        .file_with("/a.bin", test_data(2_000_000), |f| {
            f.delay_per_chunk = Some(std::time::Duration::from_millis(20));
        })
        .start();

    let mut settings = manager.settings();
    settings.max_concurrent = 8;
    manager.set_settings(settings);

    let mut queue = Queue::new("narrow", "Narrow");
    queue.concurrency = 1;
    manager.put_queue(queue).unwrap();

    let mut ids = Vec::new();
    for _ in 0..3 {
        let id = manager.add(
            DownloadSpec::new(&server.url("/a.bin"), dir.path().to_path_buf()),
            None,
            true,
        );
        manager.set_queue(&id, Some("narrow")).unwrap();
        ids.push(id);
    }

    let mut ever_ran = false;
    for _ in 0..12 {
        manager.tick();
        std::thread::sleep(std::time::Duration::from_millis(120));
        let running = ids
            .iter()
            .filter(|id| {
                manager
                    .snapshot_one(id)
                    .map(|s| s.str_or("status", "") == "downloading")
                    == Some(true)
            })
            .count();
        assert!(running <= 1, "{running} running in a queue limited to 1");
        ever_ran |= running == 1;
    }
    // Without this the limit would be satisfied trivially by nothing running.
    assert!(
        ever_ran,
        "no download ever started, so the limit was never exercised"
    );
}

#[test]
fn a_new_download_reports_its_queue_through_the_api() {
    let (manager, dir) = manager();
    let id = manager.add(
        DownloadSpec::new("https://example.com/f.bin", dir.path().to_path_buf()),
        None,
        false,
    );
    // Unassigned downloads report no queue and are treated as main.
    let snapshot: Json = manager.snapshot_one(&id).unwrap();
    assert!(snapshot.get("queue").is_some());
}

#[test]
fn settings_round_trip_with_queues() {
    let dir = TempDir::new("hydra-qsettings").unwrap();
    let path = dir.path().join("state.json");
    {
        let mut store = Store::load(&path, dir.path().to_path_buf());
        let mut queue = Queue::new("night", "Night");
        queue.completion = Completion::Sleep;
        queue.schedule = Schedule {
            enabled: true,
            start: 1380,
            stop: Some(360),
            days: EVERY_DAY,
        };
        store.settings.queues.push(queue);
        store.save().unwrap();
    }
    let reloaded = Store::load(&path, PathBuf::from("/unused"));
    let night = reloaded
        .settings
        .queues
        .iter()
        .find(|q| q.id == "night")
        .unwrap();
    assert_eq!(night.completion, Completion::Sleep);
    assert!(night.schedule.enabled);
    assert_eq!(night.schedule.stop, Some(360));
}
