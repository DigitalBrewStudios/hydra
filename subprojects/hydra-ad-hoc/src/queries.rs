//! The daemon's own reads and writes of Hydra's `Builds` table.
//!
//! These queries are compile-time checked against the shared schema
//! but belong to this daemon alone, so they live here, on the raw
//! connections the `db` crate hands out for that purpose.

use std::collections::BTreeMap;

use harmonia_store_derivation::derived_path::OutputName;
use harmonia_store_path::{StoreDir, StorePath};
use sqlx::Connection as _;

use db::models::{BuildID, BuildStatus};

/// A build the queue runner has finished, as the daemon reports it
/// back to the client.
#[derive(Debug)]
pub(crate) struct FinishedBuild {
    pub(crate) status: BuildStatus,
    pub(crate) start_time: Option<db::Timestamp>,
    pub(crate) stop_time: Option<db::Timestamp>,
    /// From `BuildOutputs`, which the queue runner fills in for every
    /// successful build, cached or not. Empty for a failed one.
    pub(crate) outputs: BTreeMap<OutputName, StorePath>,
}

/// The finished build with `build_id`, or `None` while it is still
/// unfinished.
pub(crate) async fn get_finished_build(
    conn: &mut sqlx::PgConnection,
    store_dir: &StoreDir,
    build_id: BuildID,
) -> Result<Option<FinishedBuild>, db::Error> {
    let Some(row) = sqlx::query!(
        "SELECT buildStatus, startTime, stopTime
         FROM builds
         WHERE id = $1 AND finished = 1",
        build_id,
    )
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let Some(status) = row.buildstatus.and_then(BuildStatus::from_i32) else {
        return Ok(None);
    };

    let outputs = sqlx::query!(
        "SELECT name, path FROM buildoutputs WHERE build = $1 AND path IS NOT NULL",
        build_id,
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .filter_map(|r| r.path.map(|p| (r.name, p)))
    .map(|(name, path)| -> Result<_, db::Error> {
        let name: OutputName = name.parse()?;
        let path: StorePath = store_dir.parse(&path)?;
        Ok((name, path))
    })
    .collect::<Result<_, _>>()?;

    Ok(Some(FinishedBuild {
        status,
        start_time: row.starttime,
        stop_time: row.stoptime,
        outputs,
    }))
}

/// Which of `ids` are finished. For the waiter's sweep after a lost
/// notification listener: PostgreSQL does not replay missed
/// notifications, so registered builds are re-checked directly.
pub(crate) async fn finished_build_ids(
    conn: &mut sqlx::PgConnection,
    ids: &[BuildID],
) -> Result<Vec<BuildID>, db::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(sqlx::query_scalar!(
        "SELECT id FROM builds WHERE id = ANY($1) AND finished = 1",
        ids,
    )
    .fetch_all(conn)
    .await?)
}

/// Cancel still-unfinished builds: mark them finished with status
/// `Cancelled`, the same shape the web UI's `cancelBuilds` writes.
///
/// The schema's `BuildCancelled` trigger then tells the queue runner
/// to abort whatever it is still building for these rows.
/// `build_finished` is notified here for the same reason the queue
/// runner notifies it when it finalises a row itself: whoever is
/// waiting on the build must wake even when the queue runner will
/// never touch the row again, because nobody is building it anymore.
///
/// Returns the ids it actually cancelled; already-finished rows are
/// left alone.
pub(crate) async fn cancel_builds(
    conn: &mut sqlx::PgConnection,
    ids: &[BuildID],
) -> Result<Vec<BuildID>, db::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let now = jiff::Timestamp::now().as_second();
    // The update and the notifications commit together, so nobody can
    // observe a cancelled row without the wakeup that goes with it.
    let mut tx = conn.begin().await?;
    let cancelled = sqlx::query_scalar!(
        "UPDATE Builds
         SET finished = 1, buildStatus = $2, startTime = $3, stopTime = $3
         WHERE id = ANY($1) AND finished = 0
         RETURNING id",
        ids,
        BuildStatus::Cancelled as i32,
        now,
    )
    .fetch_all(&mut *tx)
    .await?;
    for build_id in &cancelled {
        // The queue runner's `build_finished` payload format: the id,
        // then the ids of dependent builds that finished with it. A
        // cancelled build takes none with it.
        sqlx::query!("SELECT pg_notify('build_finished', $1::text)", build_id.to_string())
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup() -> (test_utils::TestPg, db::Database) {
        let (pg, _pool) = test_utils::TestPg::new().await;
        let db = db::Database::new(&pg.url(), 2).await.unwrap();
        (pg, db)
    }

    /// File a build the way the daemon does, so there is a row to
    /// cancel.
    async fn submit_build(db: &db::Database) -> BuildID {
        let submitter = crate::submit::AdhocSubmitter::new(db.clone()).await.unwrap();
        let mut conn = db.get().await.unwrap();
        let mut tx = conn.raw().begin().await.unwrap();
        let id = submitter
            .submit(
                &mut tx,
                crate::submit::BuildRequest {
                    drv_path: "/nix/store/foo.drv",
                    nix_name: "hello",
                    system: "x86_64-linux",
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    }

    #[tokio::test]
    async fn cancel_marks_unfinished_build_cancelled() {
        let (_pg, db) = setup().await;
        let id = submit_build(&db).await;
        let mut conn = db.get().await.unwrap();
        let cancelled = cancel_builds(conn.raw(), &[id]).await.unwrap();
        assert_eq!(cancelled, vec![id]);
        let row = sqlx::query!(
            "SELECT finished, buildStatus, startTime, stopTime
             FROM Builds WHERE id = $1",
            id,
        )
        .fetch_one(conn.raw())
        .await
        .unwrap();
        assert_eq!(row.finished, 1);
        assert_eq!(row.buildstatus, Some(BuildStatus::Cancelled as i32));
        assert_eq!(row.starttime, row.stoptime, "start and stop together");
    }

    #[tokio::test]
    async fn cancel_leaves_finished_builds_alone() {
        let (_pg, db) = setup().await;
        let id = submit_build(&db).await;
        let mut conn = db.get().await.unwrap();
        // The queue runner got there first and marked it a success.
        sqlx::query!(
            "UPDATE Builds
             SET finished = 1, buildStatus = $2, startTime = $3, stopTime = $3
             WHERE id = $1",
            id,
            BuildStatus::Success as i32,
            jiff::Timestamp::now().as_second(),
        )
        .execute(conn.raw())
        .await
        .unwrap();
        let cancelled = cancel_builds(conn.raw(), &[id]).await.unwrap();
        assert!(cancelled.is_empty(), "already-finished rows are untouched");
        let row = sqlx::query!(
            "SELECT buildStatus FROM Builds WHERE id = $1",
            id,
        )
        .fetch_one(conn.raw())
        .await
        .unwrap();
        assert_eq!(row.buildstatus, Some(BuildStatus::Success as i32));
    }
}
