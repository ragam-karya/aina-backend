// The daily rollup of the analytics (docs/analytics.md): each morning the Worker asks Workers Analytics
// Engine (which keeps three months) for yesterday's numbers and keeps them for good in D1's daily_stats,
// one row per (day, metric, dimension). Days are WIB days. This file holds the rules (the day's window,
// the queries, reading their answers); the Worker does the HTTP and D1.

use serde_json::Value;

use crate::plans::civil_at;

pub const DAY_MS: i64 = 86_400_000;
const WIB: i64 = 7 * 3600;

/// How many past days the rollup looks back over for a day it hasn't kept yet (a missed cron, a failed
/// query): Analytics Engine still has them.
pub const CATCH_UP_DAYS: i64 = 7;

/// Days rolled up per run at most: a day takes one Analytics Engine query per metric (QUERIES), and the
/// Workers Free plan allows 50 subrequests per run. A later run (or POST /admin/stats/roll) does the rest.
pub const DAYS_PER_RUN: usize = 1;

/// The metric that marks a day as rolled up.
pub const ROLLED: &str = "rolled";

/// The UTC instant (ms) at which the WIB day holding `ms` began.
pub fn wib_day_start(ms: i64) -> i64 {
    (ms + WIB * 1000).div_euclid(DAY_MS) * DAY_MS - WIB * 1000
}

/// "2026-11-09": the WIB day holding `ms`.
pub fn wib_day(ms: i64) -> String {
    let (y, m, d, _) = civil_at(ms, WIB);
    format!("{y:04}-{m:02}-{d:02}")
}

/// "2026-11-09 17:00:00": `ms` in UTC, as Analytics Engine's SQL reads a time.
pub fn utc_stamp(ms: i64) -> String {
    let (y, m, d, sec) = civil_at(ms, 0);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}", sec / 3600, sec % 3600 / 60, sec % 60)
}

/// The days to roll up on a run at `now`, newest first: yesterday and the CATCH_UP_DAYS before it, as
/// (day, start ms). Today isn't over yet.
pub fn days_to_check(now: i64) -> Vec<(String, i64)> {
    let today = wib_day_start(now);
    (1..=CATCH_UP_DAYS).map(|back| today - back * DAY_MS).map(|start| (wib_day(start), start)).collect()
}

const WEB: &str = "aina_events";
const AI: &str = "aina_ai_calls";

// Where a page view came from: the campaign tag, else the other site, else typed in or bookmarked.
const SOURCE: &str = "if(blob4 != '', blob4, if(blob3 != '', blob3, '(langsung)'))";

/// The rollup's queries: (metric, dataset, filter, dimensions, value). Every row a query gives becomes one
/// daily_stats row; its dimensions are joined with ':' ("" without any).
const QUERIES: [(&str, &str, &str, &[&str], &str); 18] = [
    ("visits", WEB, "blob1 = 'pageview' AND blob9 = '0'", &[], "SUM(_sample_interval)"),
    ("visitors", WEB, "blob1 = 'pageview' AND blob9 = '0'", &[], "COUNT(DISTINCT index1)"),
    ("downloads", WEB, "blob1 = 'download' AND double2 = 200 AND blob9 = '0'", &[], "SUM(_sample_interval)"),
    ("downloaders", WEB, "blob1 = 'download' AND double2 = 200 AND blob9 = '0'", &[], "COUNT(DISTINCT index1)"),
    ("bot_hits", WEB, "blob9 = '1'", &[], "SUM(_sample_interval)"),
    ("visits_by_source", WEB, "blob1 = 'pageview' AND blob9 = '0'", &[SOURCE], "SUM(_sample_interval)"),
    ("visits_by_page", WEB, "blob1 = 'pageview' AND blob9 = '0'", &["blob2"], "SUM(_sample_interval)"),
    ("visits_by_country", WEB, "blob1 = 'pageview' AND blob9 = '0'", &["blob7"], "SUM(_sample_interval)"),
    ("visits_by_device", WEB, "blob1 = 'pageview' AND blob9 = '0'", &["blob8"], "SUM(_sample_interval)"),
    ("downloads_by_source", WEB, "blob1 = 'download' AND double2 = 200 AND blob9 = '0'", &[SOURCE], "SUM(_sample_interval)"),
    ("downloads_by_os", WEB, "blob1 = 'download' AND double2 = 200 AND blob9 = '0'", &["blob11"], "SUM(_sample_interval)"),
    ("ai_users", AI, "blob5 = 'ok'", &[], "COUNT(DISTINCT index1)"),
    ("ai_calls", AI, "blob5 = 'ok'", &["blob1", "blob2"], "SUM(_sample_interval)"),
    // double8 is milli-rupiah of API cost (HPP); double11 the milli-credits taken (from 2026-11-14).
    ("ai_cost_idr", AI, "blob5 = 'ok'", &["blob1", "blob2"], "SUM(_sample_interval * double8) / 1000"),
    ("ai_credits", AI, "blob5 = 'ok'", &["blob1", "blob2"], "SUM(_sample_interval * double11) / 1000"),
    ("ai_calls_by_feature", AI, "blob5 = 'ok'", &["blob3"], "SUM(_sample_interval)"),
    ("ai_first_ms_p50", AI, "blob5 = 'ok' AND double9 > 0", &["blob1", "blob2"], "quantileWeighted(0.5)(double9, _sample_interval)"),
    ("ai_problems", AI, "blob5 != 'ok'", &["blob5", "blob6", "double2"], "SUM(_sample_interval)"),
];

/// The rollup's queries for the WIB day starting at `start`: (metric, SQL).
pub fn queries(start: i64) -> Vec<(&'static str, String)> {
    let (from, to) = (utc_stamp(start), utc_stamp(start + DAY_MS));
    QUERIES
        .iter()
        .map(|(metric, dataset, filter, dims, value)| {
            let cols: String = dims.iter().enumerate().map(|(i, d)| format!("{d} AS d{i}, ")).collect();
            let group = if dims.is_empty() { String::new() } else { format!(" GROUP BY {}", (0..dims.len()).map(|i| format!("d{i}")).collect::<Vec<_>>().join(", ")) };
            let sql = format!(
                "SELECT {cols}{value} AS v FROM {dataset} WHERE timestamp >= toDateTime('{from}') AND timestamp < toDateTime('{to}') AND {filter}{group} FORMAT JSON"
            );
            (*metric, sql)
        })
        .collect()
}

/// The (dimension, value) rows of an Analytics Engine SQL answer (FORMAT JSON) to one of `queries`. Its
/// numbers may come as strings (64-bit integers do); rows without a number are left out.
pub fn rows(answer: &Value) -> Vec<(String, f64)> {
    let num = |v: &Value| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
    let text = |v: &Value| match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => num(other).map(|n| if n.fract() == 0.0 { format!("{}", n as i64) } else { n.to_string() }).unwrap_or_default(),
    };
    let Some(data) = answer["data"].as_array() else { return vec![] };
    data.iter()
        .filter_map(|row| {
            let v = num(&row["v"]).filter(|v| v.is_finite())?;
            let dims: Vec<String> = (0..).map(|i| format!("d{i}")).take_while(|k| row.get(k).is_some()).map(|k| text(&row[&k])).collect();
            Some((dims.join(":").chars().take(200).collect(), v))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // 2026-11-10 08:00 WIB = 2026-11-10 01:00 UTC, when the cron runs.
    const CRON: i64 = 1_794_272_400_000;

    #[test]
    fn days_are_wib_days() {
        assert_eq!(utc_stamp(CRON), "2026-11-10 01:00:00");
        assert_eq!(wib_day(CRON), "2026-11-10");
        // A WIB day starts at 17:00 UTC the day before.
        assert_eq!(utc_stamp(wib_day_start(CRON)), "2026-11-09 17:00:00");
        // 23:30 WIB is still the same day; 00:30 WIB the next.
        let late = wib_day_start(CRON) + DAY_MS - 1_800_000;
        assert_eq!(wib_day(late), "2026-11-10");
        assert_eq!(wib_day(late + 3_600_000), "2026-11-11");
    }

    #[test]
    fn the_run_checks_yesterday_and_the_week_before_it() {
        let days = days_to_check(CRON);
        assert_eq!(days.len() as i64, CATCH_UP_DAYS);
        assert_eq!(days[0].0, "2026-11-09");
        assert_eq!(utc_stamp(days[0].1), "2026-11-08 17:00:00");
        assert_eq!(days.last().unwrap().0, "2026-11-03");
    }

    #[test]
    fn a_query_covers_exactly_its_day() {
        let q = queries(days_to_check(CRON)[0].1);
        assert_eq!(q.len(), QUERIES.len());
        let (metric, sql) = &q[0];
        assert_eq!(*metric, "visits");
        assert!(sql.contains("timestamp >= toDateTime('2026-11-08 17:00:00') AND timestamp < toDateTime('2026-11-09 17:00:00')"), "{sql}");
        assert!(!sql.contains("GROUP BY"));
        let (_, by) = q.iter().find(|(m, _)| *m == "ai_calls").unwrap();
        assert!(by.contains("blob1 AS d0, blob2 AS d1,") && by.ends_with("GROUP BY d0, d1 FORMAT JSON"), "{by}");
    }

    #[test]
    fn answers_become_rows() {
        let a = json!({ "data": [
            { "d0": "messages", "d1": "claude-sonnet-5-5", "v": "42" },
            { "d0": "upstream", "d1": "", "d2": 529, "v": 3.0 },
            { "d0": "x", "v": null },
        ]});
        assert_eq!(rows(&a), vec![("messages:claude-sonnet-5-5".to_string(), 42.0), ("upstream::529".to_string(), 3.0)]);
        assert_eq!(rows(&json!({ "data": [{ "v": "7" }] })), vec![(String::new(), 7.0)]);
        assert!(rows(&json!({ "errors": ["bad"] })).is_empty());
    }
}
