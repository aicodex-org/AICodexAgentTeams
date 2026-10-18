//! DeepSeek 官方分时价目、显式覆盖与完整 Token 用量的 USD 费用计算

use std::collections::BTreeMap;

use gateway_core::metering::{
    CalculatedCost, CalculatedCostAmounts, CalculatedCostBreakdown, CalculatedCostRates,
    CurrencyCode, Decimal, ModelPriceOverride, Money, Usage,
};

// 官方美元价目，单位为每 Token 的 10^-10 USD，高峰价为闲时价的两倍
// https://api-docs.deepseek.com/quick_start/pricing/
const FLASH_MODELS: &[&str] = &[
    "deepseek-flash",
    "deepseek-v4-flash",
    "deepseek-v4-flash-vision-exp",
];
const PRO_MODELS: &[&str] = &["deepseek-v4-pro"];
const PRICES: &[(&[&str], [u128; 3])] = &[
    (FLASH_MODELS, [3_000, 12_000, 60]),
    (PRO_MODELS, [13_200, 39_600, 440]),
];

pub(crate) fn catalog() -> BTreeMap<String, ModelPriceOverride> {
    PRICES
        .iter()
        .flat_map(|(models, ticks)| {
            models
                .iter()
                .map(|model| ((*model).to_owned(), model_price(*ticks)))
        })
        .collect()
}

// 人工项逐档覆盖内置价，缺失的空闲档不能回退到高峰档
pub(crate) fn resolve(
    model: &str,
    configured: Option<&ModelPriceOverride>,
    off_peak: Option<bool>,
) -> Option<ModelPriceOverride> {
    let band = if off_peak? { "off_peak" } else { "standard" };
    let builtin = PRICES.iter().find(|(models, _)| models.contains(&model));
    let rates = configured
        .and_then(|price| price.bands.get(band).cloned())
        .or_else(|| builtin.map(|(_, ticks)| model_price(*ticks).bands[band].clone()))?;
    Some(ModelPriceOverride {
        multiplier_bps: configured.map_or(10_000, |price| price.multiplier_bps),
        // 有效档位归一后冻结，后续费用计算不再依赖响应完成时间
        bands: BTreeMap::from([("standard".to_owned(), rates)]),
    })
}

fn model_price(ticks: [u128; 3]) -> ModelPriceOverride {
    let band = |divisor: u128| {
        let price = |ticks: u128| {
            Decimal::from_scaled(ticks * 1_000_000 / divisor)
                .expect("内置单价在范围内")
                .canonical()
                .try_into()
                .expect("内置单价精度合法")
        };
        gateway_core::metering::TokenPriceOverride {
            input: price(ticks[0]),
            output: price(ticks[1]),
            cache_read: price(ticks[2]),
            cache_write: price(ticks[0]),
        }
    };
    ModelPriceOverride {
        multiplier_bps: 10_000,
        bands: BTreeMap::from([
            ("standard".to_owned(), band(1)),
            ("off_peak".to_owned(), band(2)),
        ]),
    }
}

pub(crate) fn calculate(usage: &Usage, pricing: &ModelPriceOverride) -> Option<CalculatedCost> {
    let rates = pricing.bands.get("standard")?;
    let cached = usage.cached_input_tokens.unwrap_or(0);
    let written = usage.cache_write_input_tokens.unwrap_or(0);
    let input = usage
        .input_tokens?
        .checked_sub(cached)?
        .checked_sub(written)?;
    let amount = |tokens: u64, price: gateway_core::metering::TokenPrice| {
        u128::from(tokens).checked_mul(price.ticks_per_token())
    };
    let input = amount(input, rates.input)?;
    let output = amount(usage.output_tokens?, rates.output)?;
    let cached = amount(cached, rates.cache_read)?;
    let written = amount(written, rates.cache_write)?;
    let total = input
        .checked_add(output)?
        .checked_add(cached)?
        .checked_add(written)?;
    let per_million = |price: gateway_core::metering::TokenPrice| {
        usd(price.ticks_per_token().checked_mul(1_000_000)?)
    };
    Some(
        CalculatedCostBreakdown::new(
            CalculatedCostAmounts::new(
                usd(input)?,
                usd(output)?,
                usd(cached)?,
                usd(written)?,
                usd(total)?,
                usd(total)?,
            ),
            CalculatedCostRates::new(
                per_million(rates.input)?,
                per_million(rates.output)?,
                per_million(rates.cache_read)?,
                per_million(rates.cache_write)?,
            ),
            None,
            100,
        )
        .with_custom_multiplier(pricing.multiplier_bps)?
        .calculated_cost(),
    )
}

fn usd(ticks: u128) -> Option<Money> {
    Some(Money::new(
        Decimal::from_scaled(ticks).ok()?,
        CurrencyCode::new("USD").ok()?,
    ))
}
