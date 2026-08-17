//! Nightly scheduler: creates the nightly run once per UTC date at the configured time.

use crate::api::admin::parse_hhmm;
use crate::models::RunKind;
use crate::runs::{create_run, RunRequest};
use crate::store::Store;
use chrono::{Duration as ChronoDuration, NaiveTime, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Window after the scheduled time during which a missed nightly is still started.
const CATCH_UP_HOURS: i64 = 12;

/// Returns the nightly date string to run now, if any.
pub fn due_nightly_date(store: &Store) -> anyhow::Result<Option<String>> {
    let settings = store.settings()?;
    if !settings.nightly_enabled {
        return Ok(None);
    }
    let Some((h, m)) = parse_hhmm(&settings.nightly_time_utc) else {
        return Ok(None);
    };
    let now = Utc::now();
    let time = NaiveTime::from_hms_opt(h, m, 0).unwrap();
    // Candidate: today's slot, or yesterday's if we're within the catch-up window.
    for days_back in 0..2 {
        let date = (now - ChronoDuration::days(days_back)).date_naive();
        let scheduled = date.and_time(time).and_utc();
        if now >= scheduled && now < scheduled + ChronoDuration::hours(CATCH_UP_HOURS) {
            let key = date.format("%Y-%m-%d").to_string();
            let exists = store
                .list_runs(None, Some(RunKind::Nightly), 400, None)?
                .iter()
                .any(|r| r.nightly_date.as_deref() == Some(key.as_str()));
            if !exists {
                return Ok(Some(key));
            }
        }
    }
    Ok(None)
}

pub async fn run_scheduler(store: Arc<Store>, poll_secs: u64, mut shutdown: watch::Receiver<bool>) {
    let poll = Duration::from_secs(poll_secs.max(5));
    tracing::info!("nightly scheduler started");
    loop {
        if *shutdown.borrow() {
            break;
        }
        match due_nightly_date(&store) {
            Ok(Some(date)) => {
                let settings = store.settings().unwrap_or_default();
                match create_run(
                    &store,
                    &settings,
                    RunRequest {
                        kind: RunKind::Nightly,
                        series_length: settings.nightly_series_length,
                        created_by: "scheduler".into(),
                        nightly_date: Some(date.clone()),
                        seed: None,
                    },
                ) {
                    Ok(run) => {
                        tracing::info!(run = %run.id, %date, participants = run.participants.len(), "nightly run created")
                    }
                    Err(e) => tracing::warn!(error = %e, %date, "nightly run not created"),
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "scheduler check failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(poll) => {}
            _ = shutdown.changed() => {}
        }
    }
}
