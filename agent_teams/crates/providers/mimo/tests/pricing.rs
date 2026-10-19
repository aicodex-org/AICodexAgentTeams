//! MiMo 官方 v2.6 美元档位、缓存抵扣与缺失用量边界测试
#[path = "../src/pricing.rs"]
mod implementation;

use gateway_core::metering::{ModelPriceOverride, TokenPriceOverride, Usage};
use std::collections::BTreeMap;

#[test]
fn official_catalog_contains_only_confirmed_v26_realtime_usd_prices() {
    let catalog = implementation::catalog();
    assert_eq!(catalog.len(), 3);
    for (model, rates) in [
        ("mimo-v2.6-pro", ["0.435", "0.87", "0.0036"]),
        ("mimo-v2.6-flash", ["0.14", "0.28", "0.0028"]),
        ("mimo-v2.6-pro-ultraspeed", ["4.35", "8.7", "0.036"]),
    ] {
        let model = &catalog[model];
        assert_eq!(model.bands.len(), 1);
        let price = &model.bands["standard"];
        assert_eq!(price.input, rates[0].to_owned().try_into().unwrap());
        assert_eq!(price.output, rates[1].to_owned().try_into().unwrap());
        assert_eq!(price.cache_read, rates[2].to_owned().try_into().unwrap());
        assert_eq!(price.cache_write, "0".to_owned().try_into().unwrap());
    }
    assert!(implementation::resolve("mimo-v2.5", None).is_none());
    assert!(implementation::resolve("mimo-unknown", None).is_none());
}

#[test]
fn overrides_and_complete_cache_usage_determine_cost_without_guessing_missing_tokens() {
    let configured = ModelPriceOverride {
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
    };
    let price = implementation::resolve("mimo-unknown", Some(&configured)).unwrap();
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
