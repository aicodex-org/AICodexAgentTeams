//! DeepSeek 账号峰谷计价日历与配置校验，不参与账号调度

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct PricingSchedule {
    enabled: bool,
    timezone: Tz,
    peak_weekdays: Vec<u32>,
    peak_windows: Vec<PeakWindow>,
    exclude_chinese_holidays: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeakWindow {
    start: String,
    end: String,
}

impl Default for PricingSchedule {
    fn default() -> Self {
        Self {
            enabled: true,
            timezone: chrono_tz::Asia::Shanghai,
            peak_weekdays: vec![1, 2, 3, 4, 5],
            peak_windows: vec![
                PeakWindow {
                    start: "09:00".to_owned(),
                    end: "12:00".to_owned(),
                },
                PeakWindow {
                    start: "14:00".to_owned(),
                    end: "18:00".to_owned(),
                },
            ],
            exclude_chinese_holidays: true,
        }
    }
}

impl PricingSchedule {
    pub(crate) fn validate(&self) -> Result<(), ()> {
        if self.peak_weekdays.is_empty()
            || self.peak_weekdays.len() > 7
            || self.peak_weekdays.iter().any(|day| !(1..=7).contains(day))
            || self.peak_windows.is_empty()
            || self.peak_windows.len() > 8
        {
            return Err(());
        }
        let mut weekdays = self.peak_weekdays.clone();
        weekdays.sort_unstable();
        weekdays.dedup();
        if weekdays.len() != self.peak_weekdays.len() {
            return Err(());
        }
        let mut windows = self
            .peak_windows
            .iter()
            .map(|window| {
                let start = minute(&window.start, false).ok_or(())?;
                let end = minute(&window.end, true).ok_or(())?;
                if start >= end {
                    return Err(());
                }
                Ok((start, end))
            })
            .collect::<Result<Vec<_>, ()>>()?;
        windows.sort_unstable();
        if windows.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(());
        }
        Ok(())
    }

    pub(crate) fn is_off_peak(&self, at: DateTime<Utc>) -> Option<bool> {
        if !self.enabled {
            return Some(true);
        }
        let local = at.with_timezone(&self.timezone);
        let clock = local.hour() * 60 + local.minute();
        let peak = self
            .peak_weekdays
            .contains(&local.weekday().number_from_monday())
            && self.peak_windows.iter().any(|window| {
                minute(&window.start, false).is_some_and(|start| clock >= start)
                    && minute(&window.end, true).is_some_and(|end| clock < end)
            });
        if !peak || !self.exclude_chinese_holidays {
            return Some(!peak);
        }
        // 官方假期按北京时间判断，与自定义窗口所在时区独立
        let china = at.with_timezone(&chrono_tz::Asia::Shanghai);
        // 未登记年度不能猜测法定节假日，不产生可能错误的高峰金额
        if china.year() != 2026 {
            return None;
        }
        // 国办发明电〔2025〕7号，周末始终按配置 weekday 处理，不添加调休工作日
        // https://www.gov.cn/zhengce/content/202511/content_7047090.htm
        Some(matches!(
            (china.month(), china.day()),
            (1, 1..=3)
                | (2, 15..=23)
                | (4, 4..=6)
                | (5, 1..=5)
                | (6, 19..=21)
                | (9, 25..=27)
                | (10, 1..=7)
        ))
    }
}

fn minute(value: &str, allow_day_end: bool) -> Option<u32> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return None;
    }
    let hour = u32::from(bytes[0] - b'0') * 10 + u32::from(bytes[1] - b'0');
    let minute = u32::from(bytes[3] - b'0') * 10 + u32::from(bytes[4] - b'0');
    if minute >= 60 || hour > 24 || (hour == 24 && (!allow_day_end || minute != 0)) {
        return None;
    }
    Some(hour * 60 + minute)
}
