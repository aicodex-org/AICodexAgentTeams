//! 已核实 Jev 标准价和缺失用量的费用边界
#[path = "../src/pricing.rs"]
mod implementation;
use gateway_core::metering::Usage;
use implementation::*;
#[test]
fn absent_output_does_not_erase_known_free_output_charge() {
    let usage = Usage {
        input_tokens: Some(1_000_000),
        ..Usage::new()
    };
    let price = resolve("jev-latest", None).unwrap();
    assert_eq!(
        calculate(&usage, &price)
            .unwrap()
            .total()
            .amount()
            .canonical(),
        "0.042"
    );
    assert_eq!(usage.output_tokens, None);
    let mut paid_output = price;
    paid_output.bands.get_mut("standard").unwrap().output = "1".to_owned().try_into().unwrap();
    assert!(calculate(&usage, &paid_output).is_none());
    assert!(resolve("future-unknown-version", None).is_none());
}
