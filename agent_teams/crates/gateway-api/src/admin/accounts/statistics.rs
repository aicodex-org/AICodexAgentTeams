//! 单账号统计的管理鉴权入口和部署时区展示投影

use super::*;
use crate::auth::SessionState;
use gateway_admin::model::observability::AccountStatisticsPeriod;

pub(super) async fn account_statistics<S>(
    _auth: AdminAuth,
    State(state): State<S>,
    axum::extract::Path(account_id): axum::extract::Path<String>,
) -> Result<impl IntoResponse, AdminError>
where
    S: SessionState + Send + Sync,
{
    require_account_id(&account_id, "accountId").map_err(map_wire_error)?;
    let services = state.admin_services();
    let result = services
        .observability()
        .account_statistics(&account_id)
        .await
        .map_err(map_service_error)?;
    let timezone = services.timezone();
    let time = crate::time::TimePresenter::new(timezone);
    let data = serde_json::json!({
        "accountId": result.account_id,
        "sevenDays": period_view(result.seven_days, time),
        "thirtyDays": period_view(result.thirty_days, time),
        "activeDays": result.active_days,
        "dailyAverageRequests": result.daily_average_requests,
        "dailyAverageCostUsd": result.daily_average_cost_usd.map(|amount| amount.as_str().to_owned()),
        "generatedAtDisplay": time.datetime(&result.analytics.generated_at),
        "timezone": timezone.name(),
        "analytics": super::super::observability::analytics_view(result.analytics, time),
    });
    Ok(AdminResponse::new(StatusCode::OK, AdminEnvelope::ok(data)))
}

fn period_view(value: AccountStatisticsPeriod, time: crate::time::TimePresenter) -> Value {
    serde_json::json!({
        "start": value.range.start, "end": value.range.end,
        "startDisplay": time.datetime(&value.range.start), "endDisplay": time.datetime(&value.range.end),
        "requestCount": value.request_count, "costUsd": value.cost_usd.map(|amount| amount.as_str().to_owned()),
        "knownCostCount": value.known_cost_count, "unknownCostCount": value.unknown_cost_count,
        "partialCostCount": value.partial_cost_count, "notBillableCount": value.not_billable_count,
        "nonUsdCostPresent": value.non_usd_cost_present,
    })
}
