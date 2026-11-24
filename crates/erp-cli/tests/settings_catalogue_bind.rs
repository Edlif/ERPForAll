// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Settings catalogue binding of a process: the first system is kept; the same system, or one over the same
//! addon directories, binds again without error; a system over other directories is a programming error that
//! names both.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration assertions"
)]
use erp_cli::module::bind_settings_catalogue;
use erp_fields::ErrorKind;
use erp_modules::AddonIndex;
use erp_modules::system::ModuleSystem;
use erp_testkit::TestDb;
use std::path::Path;
use std::sync::Arc;

fn system_over(db: &TestDb, dir: &Path) -> Arc<ModuleSystem> {
    let mut index = AddonIndex::new(true);
    index.add_dev_path(dir);
    Arc::new(
        ModuleSystem::builder(db.pool().clone(), index)
            .build()
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_bind_is_kept_for_the_same_system_or_directories_and_refused_otherwise() {
    let db = TestDb::create().await.unwrap();
    let (first_dir, other_dir) = (
        Path::new("/settings-bind/first"),
        Path::new("/settings-bind/other"),
    );
    let first = system_over(&db, first_dir);
    bind_settings_catalogue(Arc::clone(&first)).unwrap();
    // The same system again, and another system over the same directories.
    bind_settings_catalogue(Arc::clone(&first)).unwrap();
    bind_settings_catalogue(system_over(&db, first_dir)).unwrap();
    // Another directory is refused, naming both.
    let refused = bind_settings_catalogue(system_over(&db, other_dir)).unwrap_err();
    assert_eq!(refused.kind(), ErrorKind::Programming);
    assert!(
        refused.message().contains(first_dir.to_str().unwrap()),
        "{}",
        refused.message()
    );
    assert!(
        refused.message().contains(other_dir.to_str().unwrap()),
        "{}",
        refused.message()
    );
    db.cleanup().await.unwrap();
}
