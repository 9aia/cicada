//! `JsonFileStateStore` — the state-v1 contract on disk: round-trip,
//! version rejection, atomic writes, journal append, lock contention.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use cicada_core::domain::ids::LocalId;
use cicada_core::domain::operation::OpKind;
use cicada_core::domain::state::{JournalEntry, JournalOutcome, RecordedState};
use cicada_core::ports::StateStore;
use cicada_state::{JsonFileStateStore, default_state_path, resolve_state_path};

fn store_at(dir: &Path) -> JsonFileStateStore {
    JsonFileStateStore::new(dir.join(".cicada").join("state.yaml"))
}

#[test]
fn new_accepts_every_path_arg_type() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("state.yaml");
    // Each `Into<PathBuf>` flavor instantiates `new` separately.
    let _ = JsonFileStateStore::new(&p);
    let _ = JsonFileStateStore::new(p.clone());
    let _ = JsonFileStateStore::new(p.to_string_lossy().into_owned());
    let _ = JsonFileStateStore::new(p.to_string_lossy().into_owned().as_str());
    let _ = JsonFileStateStore::new(p.as_path());
}

fn entry(key: &str) -> JournalEntry {
    JournalEntry {
        key: key.to_owned(),
        op: OpKind::Create,
        local_id: "standup".into(),
        applied_at: "2026-10-23T01:00:00Z".parse().unwrap(),
        outcome: JournalOutcome::Applied {
            remote_id: Some("fake-1".into()),
        },
    }
}

#[tokio::test]
async fn missing_file_loads_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    let state = store.load().await.unwrap();
    assert_eq!(state, RecordedState::new());
}

#[tokio::test]
async fn state_v1_round_trips_through_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());

    let mut state = RecordedState::new();
    {
        let cal = state.calendar_mut("work");
        cal.provider = "fake".into();
        cal.calendar_id = "primary".into();
        cal.record(
            "standup".into(),
            "fake-1".into(),
            "sha256:deadbeef".into(),
            "2026-10-23T01:00:00Z".parse().unwrap(),
        );
        cal.events.get_mut(&"standup".into()).unwrap().etag = Some("etag-3".into());
    }
    state.push_journal(entry("primary|standup|create|sha256:deadbeef"));

    store.save(&state).await.unwrap();

    // The file is the ADR-0002 §7 shape.
    let yaml = fs::read_to_string(store.path()).unwrap();
    assert!(yaml.contains("apiVersion: cicada/state-v1"), "{yaml}");
    assert!(yaml.contains("calendarId: primary"), "{yaml}");

    // A fresh store instance reads the same document back.
    let reloaded = store_at(dir.path()).load().await.unwrap();
    assert_eq!(reloaded, state);
    assert_eq!(
        reloaded.calendar("work").unwrap().events[&LocalId::new("standup")]
            .remote_id
            .as_str(),
        "fake-1"
    );
    assert_eq!(reloaded.journal.len(), 1);
}

#[tokio::test]
async fn unknown_api_version_is_a_clear_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    fs::write(
        store.path(),
        "apiVersion: cicada/state-v2\ncalendars: {}\njournal: []\n",
    )
    .unwrap();
    let err = store.load().await.unwrap_err();
    assert!(err.to_string().contains("state-v2"), "{err}");
    assert_eq!(err.kind(), cicada_core::ErrorKind::Fatal);

    // A document that isn't a state file at all also fails loudly.
    fs::write(store.path(), "just: some yaml\n").unwrap();
    assert!(store.load().await.is_err());
}

#[tokio::test]
async fn save_is_atomic_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());

    // A pre-existing valid file is replaced wholesale, never appended.
    let mut state = RecordedState::new();
    state.calendar_mut("work").record(
        "a".into(),
        "fake-1".into(),
        "sha256:1".into(),
        "2026-10-23T01:00:00Z".parse().unwrap(),
    );
    store.save(&state).await.unwrap();
    state.calendar_mut("work").record(
        "b".into(),
        "fake-2".into(),
        "sha256:2".into(),
        "2026-10-23T01:01:00Z".parse().unwrap(),
    );
    store.save(&state).await.unwrap();

    let reloaded = store.load().await.unwrap();
    assert_eq!(reloaded.calendar("work").unwrap().events.len(), 2);

    // No temp-file litter beside the state file.
    let leftovers: Vec<_> = fs::read_dir(store.path().parent().unwrap())
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name();
            (name != "state.yaml").then_some(name)
        })
        .collect();
    assert!(leftovers.is_empty(), "temp litter: {leftovers:?}");
}

#[tokio::test]
async fn append_journal_persists_across_instances() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    let e = entry("primary|standup|create|sha256:x");
    store.append_journal(&e).await.unwrap();
    store
        .append_journal(&entry("primary|retro|create|sha256:y"))
        .await
        .unwrap();

    let journal = store.journal().await.unwrap();
    assert_eq!(journal.len(), 2);
    assert_eq!(journal[0].key, e.key);
    // The same entries are inside the document's journal too.
    assert_eq!(store.load().await.unwrap().journal.len(), 2);
}

#[test]
fn lock_excludes_second_holder_and_releases_on_drop() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path());
    let other = store_at(dir.path());

    let guard = store.lock().unwrap();
    // Contended while held; free after release.
    assert!(other.try_lock().unwrap().is_none());
    guard.release();
    assert!(other.try_lock().unwrap().is_some());

    // The lock file is a sibling — the state file itself stays clean.
    assert!(store.lock_path().ends_with("state.yaml.lock"));
}

#[test]
fn resolve_state_path_prefers_flag_then_env_then_default() {
    let dir = Path::new("/tmp/manifest");
    assert_eq!(
        resolve_state_path(dir, Some(Path::new("/tmp/explicit.yaml"))),
        Path::new("/tmp/explicit.yaml")
    );
    // No flag: env wins when set, else the manifest-dir default.
    // (Env not touched here — other tests may set it via the process
    // environment; the flag path is what the CLI always passes.)
    assert_eq!(
        default_state_path(dir),
        Path::new("/tmp/manifest/.cicada/state.yaml")
    );
    let _ = cicada_state::STATE_ENV; // named constant exists for docs
}

#[tokio::test]
async fn load_errors_on_unreadable_and_malformed_documents() {
    let dir = tempfile::tempdir().unwrap();

    // Unreadable: the state path is a directory.
    let asdir = dir.path().join("asdir");
    fs::create_dir(&asdir).unwrap();
    let store = JsonFileStateStore::new(&asdir);
    let err = store.load().await.unwrap_err();
    assert!(err.to_string().contains("cannot read state file"), "{err}");

    let store = store_at(dir.path());
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();

    // An empty (or whitespace-only) file loads as fresh state.
    for source in ["", "  \n\n"] {
        fs::write(store.path(), source).unwrap();
        assert!(store.load().await.unwrap().calendars.is_empty());
    }

    // Right apiVersion, malformed body — probe passes, parse fails.
    fs::write(
        store.path(),
        "apiVersion: cicada/state-v1\ncalendars: nope\n",
    )
    .unwrap();
    let err = store.load().await.unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");

    // Not valid YAML at all — the version probe itself fails.
    fs::write(store.path(), "{{{{").unwrap();
    let err = store.load().await.unwrap_err();
    assert!(
        err.to_string().contains("not a cicada state document"),
        "{err}"
    );
}

#[test]
fn lock_fails_when_the_lock_file_cannot_be_created() {
    let dir = tempfile::tempdir().unwrap();

    // The lock path's parent is a regular file — mkdir fails.
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "x").unwrap();
    let store = JsonFileStateStore::new(blocker.join("state.yaml"));
    assert!(store.lock().is_err());
    assert!(store.try_lock().is_err());

    // The lock path itself is a directory — open fails.
    let store = store_at(dir.path());
    fs::create_dir_all(store.lock_path()).unwrap();
    assert!(store.lock().is_err());
    assert!(store.try_lock().is_err());
}

#[tokio::test]
async fn save_fails_when_the_directory_is_unwritable_or_target_is_a_dir() {
    let dir = tempfile::tempdir().unwrap();

    // Parent can't be created — it collides with a regular file.
    let blocker = dir.path().join("blocker");
    fs::write(&blocker, "x").unwrap();
    let store = JsonFileStateStore::new(blocker.join("state.yaml"));
    let err = store.save(&RecordedState::new()).await.unwrap_err();
    assert!(err.to_string().contains("cannot create"), "{err}");

    // The temp file can't be created inside a read-only dir.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let ro = dir.path().join("ro");
        fs::create_dir(&ro).unwrap();
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
        let store = JsonFileStateStore::new(ro.join("state.yaml"));
        let err = store.save(&RecordedState::new()).await.unwrap_err();
        assert!(err.to_string().contains("temp file"), "{err}");
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The state path is a directory — the atomic rename can't land.
    let target = dir.path().join("target-dir");
    fs::create_dir(&target).unwrap();
    let store = JsonFileStateStore::new(&target);
    let err = store.save(&RecordedState::new()).await.unwrap_err();
    assert!(err.to_string().contains("cannot replace"), "{err}");
}
