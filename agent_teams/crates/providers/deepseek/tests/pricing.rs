//! 官方双档模型价格、完整用量、人工覆盖与账号计价日历边界验证
#[path = "../src/pricing.rs"]
mod implementation;
#[path = "../src/pricing_schedule.rs"]
mod pricing_schedule;

use gateway_core::metering::{ModelPriceOverride, TokenPriceOverride, Usage};
use serde_json::json;
use std::collections::BTreeMap;

fn override_price() -> ModelPriceOverride {
    ModelPriceOverride {
        multiplier_bps: 20_000,
        bands: BTreeMap::from([(
            "standard".to_owned(),
            TokenPriceOverride {
                input: "2".to_owned().try_into().unwrap(),
                output: "4".to_owned().try_into().unwrap(),
                cache_read: "1".to_owned().try_into().unwrap(),
                cache_write: "2".to_owned().try_into().unwrap(),
            },
        )]),
    }
}

#[test]
fn official_catalog_preserves_both_rates_and_legacy_aliases() {
    let catalog = implementation::catalog();
    assert_eq!(catalog.len(), 4);
    for (model, peak, off_peak) in [
        (
            "deepseek-flash",
            ["0.3", "1.2", "0.006"],
            ["0.15", "0.6", "0.003"],
        ),
        (
            "deepseek-v4-flash",
            ["0.3", "1.2", "0.006"],
            ["0.15", "0.6", "0.003"],
        ),
        (
            "deepseek-v4-flash-vision-exp",
            ["0.3", "1.2", "0.006"],
            ["0.15", "0.6", "0.003"],
        ),
        (
            "deepseek-v4-pro",
            ["1.32", "3.96", "0.044"],
            ["0.66", "1.98", "0.022"],
        ),
    ] {
        let model = &catalog[model];
        assert_eq!(model.multiplier_bps, 10_000);
        for (band, expected) in [("standard", peak), ("off_peak", off_peak)] {
            let rates = &model.bands[band];
            for (rate, expected) in [
                (rates.input, expected[0]),
                (rates.output, expected[1]),
                (rates.cache_read, expected[2]),
            ] {
                assert_eq!(rate, expected.to_owned().try_into().unwrap());
            }
        }
    }
}

#[test]
fn overrides_merge_by_band_and_never_use_peak_for_unknown_off_peak() {
    let mut configured = override_price();
    let peak = implementation::resolve("deepseek-flash", Some(&configured), Some(false)).unwrap();
    assert_eq!(
        peak.bands["standard"].input,
        "2".to_owned().try_into().unwrap()
    );
    let off_peak =
        implementation::resolve("deepseek-flash", Some(&configured), Some(true)).unwrap();
    assert_eq!(
        off_peak.bands["standard"].input,
        "0.15".to_owned().try_into().unwrap()
    );
    assert_eq!(off_peak.multiplier_bps, 20_000);
    assert!(implementation::resolve("unknown", None, Some(false)).is_none());
    assert!(implementation::resolve("unknown", Some(&configured), Some(true)).is_none());
    assert!(implementation::resolve("deepseek-flash", Some(&configured), None).is_none());
    assert!(implementation::resolve("unknown", Some(&configured), Some(false)).is_some());
    configured
        .bands
        .insert("off_peak".to_owned(), configured.bands["standard"].clone());
    let custom = implementation::resolve("unknown", Some(&configured), Some(true)).unwrap();
    assert_eq!(
        custom.bands["standard"].input,
        "2".to_owned().try_into().unwrap()
    );
    configured.bands.clear();
    assert_eq!(
        implementation::resolve("deepseek-flash", Some(&configured), Some(false))
            .unwrap()
            .multiplier_bps,
        20_000
    );
}

#[test]
fn cost_requires_complete_consistent_usage_and_applies_multiplier() {
    let price =
        implementation::resolve("deepseek-flash", Some(&override_price()), Some(false)).unwrap();
    let mut usage = Usage::new();
    usage.input_tokens = Some(100);
    usage.output_tokens = Some(10);
    usage.cached_input_tokens = Some(20);
    assert_eq!(
        implementation::calculate(&usage, &price)
            .unwrap()
            .total()
            .amount()
            .canonical(),
        "0.00044"
    );
    usage.cache_write_input_tokens = Some(10);
    assert_eq!(
        implementation::calculate(&usage, &price)
            .unwrap()
            .total()
            .amount()
            .canonical(),
        "0.00044"
    );
    usage.cached_input_tokens = Some(101);
    assert!(implementation::calculate(&usage, &price).is_none());
    usage.cached_input_tokens = Some(20);
    usage.output_tokens = None;
    assert!(implementation::calculate(&usage, &price).is_none());
    usage.output_tokens = Some(10);
    usage.input_tokens = None;
    assert!(implementation::calculate(&usage, &price).is_none());
}

fn at(value: &str) -> chrono::DateTime<chrono::Utc> {
    value.parse().unwrap()
}

#[test]
fn default_calendar_uses_half_open_peak_windows_and_holidays() {
    let schedule = pricing_schedule::PricingSchedule::default();
    schedule.validate().unwrap();
    for (instant, expected) in [
        ("2026-10-19T00:59:59Z", Some(true)),
        ("2026-10-19T01:00:00Z", Some(false)),
        ("2026-10-19T03:59:59Z", Some(false)),
        ("2026-10-19T04:00:00Z", Some(true)),
        ("2026-10-19T06:00:00Z", Some(false)),
        ("2026-10-19T10:00:00Z", Some(true)),
        ("2026-10-18T01:00:00Z", Some(true)),
        ("2026-10-01T01:00:00Z", Some(true)),
        ("2027-01-04T01:00:00Z", None),
        ("2027-01-04T10:00:00Z", Some(true)),
    ] {
        assert_eq!(schedule.is_off_peak(at(instant)), expected, "{instant}");
    }
}

#[test]
fn account_calendar_uses_iana_dst_and_disabled_always_off_peak() {
    let mut config = json!({"enabled":true,"timezone":"America/New_York","peak_weekdays":[1,2,3,4,5,6,7],"peak_windows":[{"start":"09:00","end":"12:00"}],"exclude_chinese_holidays":false});
    let schedule: pricing_schedule::PricingSchedule =
        serde_json::from_value(config.clone()).unwrap();
    schedule.validate().unwrap();
    assert_eq!(
        schedule.is_off_peak(at("2026-01-05T14:00:00Z")),
        Some(false)
    );
    assert_eq!(
        schedule.is_off_peak(at("2026-07-06T13:00:00Z")),
        Some(false)
    );
    assert_eq!(schedule.is_off_peak(at("2026-07-06T12:59:59Z")), Some(true));
    config["enabled"] = json!(false);
    let disabled: pricing_schedule::PricingSchedule = serde_json::from_value(config).unwrap();
    assert_eq!(disabled.is_off_peak(at("2027-01-04T14:00:00Z")), Some(true));
    assert_eq!(disabled.is_off_peak(at("2026-07-06T13:00:00Z")), Some(true));
}
