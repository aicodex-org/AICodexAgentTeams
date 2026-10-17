//! 单账号统计的真实投影、部署日历、账号隔离和事实预算验收

use super::*;

#[tokio::test]
async fn account_statistics_use_calendar_days_and_preserve_unknown_costs_and_account_isolation() {
    let Some(database) = TestDatabase::create("account_statistics_calendar").await else {
        return;
    };
    let timezone = "Asia/Kathmandu"
        .parse::<gateway_core::time::DeploymentTimeZone>()
        .unwrap();
    let end = chrono::DateTime::parse_from_rfc3339("2026-10-17T12:00:00Z")
        .unwrap()
        .to_utc();
    for (id, at, account, cost) in [
        (
            "account_one",
            end - TimeDelta::days(1),
            "account_a",
            Some("2.5"),
        ),
        (
            "account_unknown",
            end - TimeDelta::days(2),
            "account_a",
            None,
        ),
        (
            "account_foreign",
            end - TimeDelta::days(1),
            "account_b",
            Some("999"),
        ),
    ] {
        super::super::analytics::seed(&database.pool, id, at, &[]).await;
        sqlx::query("update model_requests set provider_account_ref=$2,cost_amount=$3::numeric,cost_currency=case when $3::text is null then null else 'USD' end,cost_source=case when $3::text is null then 'unavailable' else 'provider_reported' end where id=$1")
            .bind(id).bind(account).bind(cost).execute(&database.pool).await.unwrap();
    }
    super::super::drain_analytics(&database.pool).await;
    let store = admin_observability_store(&database.pool).with_timezone(timezone);
    let range = admin_observability::TimeRange::calendar_at(
        admin_observability::CalendarPeriod::ThirtyDays,
        end,
        timezone,
    )
    .unwrap();
    let result = store
        .account_statistics("account_a".into(), range)
        .await
        .unwrap();
    assert_eq!(
        result
            .points
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        2
    );
    assert!(
        result
            .points
            .iter()
            .all(|point| timezone.day_start(point.bucket_start) == Some(point.bucket_start))
    );
    let costs = result
        .points
        .iter()
        .flat_map(|point| &point.costs)
        .collect::<Vec<_>>();
    assert_eq!(costs.len(), 1);
    assert_eq!(costs[0].amount.as_str(), "2.5");
    assert_eq!(
        result
            .points
            .iter()
            .map(|point| point.cost_coverage.unavailable_count)
            .sum::<u64>(),
        1
    );
    assert!(!result.analytics.percentiles_approximate);
    let empty = store
        .account_statistics("account_empty".into(), range)
        .await
        .unwrap();
    assert!(empty.points.is_empty());
    database.close().await;
}

#[tokio::test]
async fn account_statistics_reject_fact_budget_without_returning_partial_totals() {
    let Some(database) = TestDatabase::create("account_statistics_budget").await else {
        return;
    };
    let end = Utc::now();
    super::super::drain_analytics(&database.pool).await;
    let start = end - TimeDelta::seconds(1);
    // 在完成回填后制造热桶，查询必须使用有界事实，不能将截断计数当完整结果
    sqlx::query("insert into request_analytics_facts(id,started_at,provider_account_ref,outcome,operation,client_transport,source_version,attempt_count) select 'budget_'||n,$1,'account_budget','succeeded','responses','http_sse',1,1 from generate_series(1,10001)n")
        .bind(start).execute(&database.pool).await.unwrap();
    let range = admin_observability::TimeRange::new(start, end).unwrap();
    let result = admin_observability_store(&database.pool)
        .account_statistics("account_budget".into(), range)
        .await;
    let error = result.expect_err("exceeding fact budget must not return a truncated success");
    assert_eq!(
        error.kind(),
        gateway_admin::ports::store::AdminStoreErrorKind::Unavailable
    );
    sqlx::query("delete from request_analytics_facts where id='budget_10001'")
        .execute(&database.pool)
        .await
        .unwrap();
    let bounded = admin_observability_store(&database.pool)
        .account_statistics("account_budget".into(), range)
        .await
        .unwrap();
    assert_eq!(
        bounded
            .points
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        10_000
    );
    database.close().await;
}

#[tokio::test]
async fn account_existence_obeys_observability_budget_and_recovers_after_release() {
    use gateway_store::postgres::{ObservabilityQueryBudget, PgAdminObservabilityStore};
    use tokio::sync::oneshot;

    let Some(database) = TestDatabase::create("account_statistics_exists_budget").await else {
        return;
    };
    seed_observability_facts(&database.pool, Utc::now())
        .await
        .unwrap();
    let budget = ObservabilityQueryBudget::try_new(1, Duration::from_millis(100)).unwrap();
    let store = PgAdminObservabilityStore::new(database.pool.clone(), None, None, budget.clone());
    assert!(store.account_exists("acct_observe").await.unwrap());
    assert!(!store.account_exists("missing_account").await.unwrap());
    let occupied_pool = database.pool.clone();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let occupied = tokio::spawn(async move {
        budget
            .run(
                &occupied_pool,
                "occupied account test",
                |connection| async move {
                    started_tx.send(()).unwrap();
                    release_rx.await.unwrap();
                    drop(connection);
                    Ok(())
                },
            )
            .await
            .unwrap();
    });
    started_rx.await.unwrap();
    let error = store.account_exists("acct_observe").await.unwrap_err();
    assert_eq!(
        error.kind(),
        gateway_admin::ports::store::AdminStoreErrorKind::Unavailable
    );
    release_tx.send(()).unwrap();
    occupied.await.unwrap();
    assert!(store.account_exists("acct_observe").await.unwrap());
    sqlx::query("delete from provider_accounts where id='acct_observe'")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(!store.account_exists("acct_observe").await.unwrap());
    database.close().await;
}

#[tokio::test]
async fn account_statistics_preserve_dst_and_exact_calendar_range_boundaries() {
    let Some(database) = TestDatabase::create("account_statistics_dst_boundaries").await else {
        return;
    };
    let timezone = "America/New_York"
        .parse::<gateway_core::time::DeploymentTimeZone>()
        .unwrap();
    let instant = |value: &str| {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .to_utc()
    };
    let end = instant("2026-11-03T12:00:00Z");
    let thirty = admin_observability::TimeRange::calendar_at(
        admin_observability::CalendarPeriod::ThirtyDays,
        end,
        timezone,
    )
    .unwrap();
    let seven = admin_observability::TimeRange::calendar_at(
        admin_observability::CalendarPeriod::SevenDays,
        end,
        timezone,
    )
    .unwrap();
    // 秋季回拨的两个 01:30 必须归到同一使用日，边界前一微秒与终点不可混入
    for (id, at) in [
        ("outside_thirty", thirty.start - TimeDelta::microseconds(1)),
        ("at_thirty_start", thirty.start),
        ("before_seven", seven.start - TimeDelta::microseconds(1)),
        ("at_seven_start", seven.start),
        ("before_fallback", instant("2026-11-01T05:30:00Z")),
        ("after_fallback", instant("2026-11-01T06:30:00Z")),
        ("at_end", end),
    ] {
        super::super::analytics::seed(&database.pool, id, at, &[]).await;
        sqlx::query("update model_requests set provider_account_ref='account_dst',cost_amount=1,cost_currency='USD',cost_source='provider_reported' where id=$1")
            .bind(id).execute(&database.pool).await.unwrap();
    }
    super::super::drain_analytics(&database.pool).await;
    let result = admin_observability_store(&database.pool)
        .with_timezone(timezone)
        .account_statistics("account_dst".into(), thirty)
        .await
        .unwrap();
    assert_eq!(
        result
            .points
            .iter()
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        5
    );
    assert_eq!(result.points.len(), 4);
    assert_eq!(
        result
            .points
            .iter()
            .filter(|point| point.bucket_start >= seven.start)
            .map(|point| point.metrics.request_count)
            .sum::<u64>(),
        3
    );
    let repeated_day = result
        .points
        .iter()
        .find(|point| point.bucket_start == instant("2026-11-01T04:00:00Z"))
        .unwrap();
    assert_eq!(repeated_day.metrics.request_count, 2);
    assert_eq!(repeated_day.costs[0].amount.as_str(), "2");
    assert!(
        result
            .points
            .iter()
            .all(|point| timezone.day_start(point.bucket_start) == Some(point.bucket_start))
    );
    database.close().await;
}
