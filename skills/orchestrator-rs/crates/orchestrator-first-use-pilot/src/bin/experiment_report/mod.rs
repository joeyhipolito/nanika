use serde_json::{Value, json};

pub fn summarize(samples: &[Value]) -> Value {
    json!({
        "off": summarize_arm(samples, "off"),
        "on": summarize_arm(samples, "on"),
    })
}

fn summarize_arm(samples: &[Value], arm: &str) -> Value {
    let mut planned_count = 0u64;
    let mut completed_count = 0u64;
    let mut failed_count = 0u64;
    let mut not_run_count = 0u64;
    let mut quality_passed_count = 0u64;
    let mut elapsed_sum: Option<u64> = Some(0);
    let mut duration_sample_count = 0u64;

    for sample in samples {
        if sample.get("arm").and_then(|v| v.as_str()) != Some(arm) {
            continue;
        }
        planned_count += 1;

        let status = sample.get("status").and_then(|v| v.as_str());
        match status {
            Some("sample_completed") => completed_count += 1,
            Some("sample_failed") => failed_count += 1,
            Some("not-run") => not_run_count += 1,
            _ => {}
        }

        if sample.get("quality_passed").and_then(|v| v.as_bool()) == Some(true) {
            quality_passed_count += 1;
        }

        let is_finished = matches!(status, Some("sample_completed") | Some("sample_failed"));
        if is_finished {
            match sample.get("elapsed_ms").and_then(|v| v.as_u64()) {
                Some(ms) => {
                    duration_sample_count += 1;
                    elapsed_sum = elapsed_sum.and_then(|sum| sum.checked_add(ms));
                }
                None => elapsed_sum = None,
            }
        }
    }

    if duration_sample_count == 0 {
        elapsed_sum = None;
    }

    json!({
        "planned_count": planned_count,
        "completed_count": completed_count,
        "failed_count": failed_count,
        "not_run_count": not_run_count,
        "quality_passed_count": quality_passed_count,
        "observed_elapsed_ms_sum": elapsed_sum,
        "duration_sample_count": duration_sample_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_status_retains_counts_and_missing_duration_stays_null() -> Result<(), String> {
        let samples = vec![
            json!({"arm": "on", "status": "sample_completed", "elapsed_ms": 100u64}),
            json!({"arm": "on", "status": "sample_failed"}),
            json!({"arm": "on", "status": "not-run"}),
            json!({"arm": "off", "status": "sample_completed", "elapsed_ms": 50u64}),
        ];
        let report = summarize(&samples);
        let on = report.get("on").ok_or("missing on")?;
        if on.get("planned_count") != Some(&json!(3)) {
            return Err("planned_count mismatch".to_string());
        }
        if on.get("completed_count") != Some(&json!(1)) {
            return Err("completed_count mismatch".to_string());
        }
        if on.get("failed_count") != Some(&json!(1)) {
            return Err("failed_count mismatch".to_string());
        }
        if on.get("not_run_count") != Some(&json!(1)) {
            return Err("not_run_count mismatch".to_string());
        }
        if on.get("observed_elapsed_ms_sum") != Some(&json!(null)) {
            return Err("observed_elapsed_ms_sum should be null".to_string());
        }
        Ok(())
    }

    #[test]
    fn known_durations_sum_and_zero_observations_are_null() -> Result<(), String> {
        let samples = vec![
            json!({"arm": "off", "status": "sample_completed", "elapsed_ms": 10u64}),
            json!({"arm": "off", "status": "sample_failed", "elapsed_ms": 20u64}),
            json!({"arm": "on", "status": "not-run"}),
        ];
        let report = summarize(&samples);
        let off = report.get("off").ok_or("missing off")?;
        if off.get("observed_elapsed_ms_sum") != Some(&json!(30)) {
            return Err("observed_elapsed_ms_sum mismatch".to_string());
        }
        if off.get("duration_sample_count") != Some(&json!(2)) {
            return Err("duration_sample_count mismatch".to_string());
        }
        let on = report.get("on").ok_or("missing on")?;
        if on.get("observed_elapsed_ms_sum") != Some(&json!(null)) {
            return Err("expected null sum for zero observations".to_string());
        }
        if on.get("duration_sample_count") != Some(&json!(0)) {
            return Err("expected zero duration_sample_count".to_string());
        }
        Ok(())
    }
}
