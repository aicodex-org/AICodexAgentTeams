//! 请求列表的精确总数、双向分页与筛选隔离

use chrono::{TimeDelta, Utc};
use gateway_store::postgres::{
    ObservabilityPageSize, ObservabilityRange, UsageRecordFilter, UsageRecordQuery,
};

use super::{TestDatabase, analytics::seed, observability_repository};

#[tokio::test]
async fn usage_pages_preserve_order_and_exact_totals_from_both_ends() {
    let Some(database) = TestDatabase::create("usage_bidirectional_pages").await else {
        return;
    };
    let start = Utc::now() - TimeDelta::hours(1);
    seed(&database.pool, "page_00", start, &[]).await;
    sqlx::query("insert into model_requests select cloned.* from model_requests original cross join generate_series(1,22) n cross join lateral jsonb_populate_record(null::model_requests,to_jsonb(original)||jsonb_build_object('id','page_'||lpad(n::text,2,'0'),'client_api_key_ref',case when n%2=0 then 'key_analytics' else 'other_key' end)) cloned where original.id='page_00'")
        .execute(&database.pool).await.unwrap();
    let range =
        ObservabilityRange::new(start - TimeDelta::seconds(1), start + TimeDelta::seconds(1))
            .unwrap();
    let repository = observability_repository(&database.pool);
    for key in [
        None,
        Some("key_analytics"),
        Some("other_key"),
        Some("absent_key"),
    ] {
        let expected = (0..23)
            .rev()
            .filter(|n| match key {
                None => true,
                Some("key_analytics") => n % 2 == 0,
                Some("other_key") => n % 2 != 0,
                Some(_) => false,
            })
            .map(|n| format!("page_{n:02}"))
            .collect::<Vec<_>>();
        for page in 1..=5 {
            let result = repository
                .list_usage_records(UsageRecordQuery {
                    range,
                    filter: UsageRecordFilter {
                        client_api_key_ref: key.map(str::to_owned),
                        ..Default::default()
                    },
                    current_page: page,
                    page_size: ObservabilityPageSize::new(7).unwrap(),
                })
                .await
                .unwrap();
            let ids = result
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>();
            let expected_page = expected
                .iter()
                .skip((page as usize - 1) * 7)
                .take(7)
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                result.total,
                expected.len() as u64,
                "key={key:?}, page={page}"
            );
            assert_eq!(ids, expected_page, "key={key:?}, page={page}");
        }
    }
    database.close().await;
}
