//! Time zones of the browser: its own zone (`Date`) and named zones
//! (`Intl.DateTimeFormat`), as [`Zone`]s for the plain logic.

use calcraft_ui::cal::{date::days_from_civil, zone::Zone};
use js_sys::{Array, Date, Intl, Object, Reflect};
use wasm_bindgen::{JsCast, JsValue};

/// The browser's local time zone.
#[derive(Debug, Clone, Copy, Default)]
pub struct Browser;

impl Zone for Browser {
    fn offset(&self, utc: i64) -> i64 {
        let date = Date::new(&JsValue::from_f64(utc as f64 * 1000.0));
        // Minutes west of UTC.
        -(date.get_timezone_offset() as i64) * 60
    }
}

/// IANA name of the browser's zone, if it reports one.
pub fn browser_tzid() -> Option<String> {
    let format = Intl::DateTimeFormat::new(&Array::new(), &Object::new());
    let options = format.resolved_options();
    Reflect::get(&options, &JsValue::from_str("timeZone"))
        .ok()?
        .as_string()
        .filter(|z| !z.is_empty())
}

/// A named zone, evaluated by the browser's time zone database.
pub struct Named {
    format: Intl::DateTimeFormat,
}

impl Named {
    /// `None` if the browser does not know the zone.
    pub fn new(tzid: &str) -> Option<Self> {
        let options = Object::new();
        let set = |key: &str, value: &str| {
            Reflect::set(&options, &JsValue::from_str(key), &JsValue::from_str(value)).ok()
        };
        set("timeZone", tzid)?;
        set("hourCycle", "h23")?;
        for part in ["year", "month", "day", "hour", "minute", "second"] {
            set(part, "numeric")?;
        }
        let intl = Reflect::get(&js_sys::global(), &JsValue::from_str("Intl")).ok()?;
        let constructor: js_sys::Function =
            Reflect::get(&intl, &JsValue::from_str("DateTimeFormat"))
                .ok()?
                .dyn_into()
                .ok()?;
        let args = Array::of2(&JsValue::from_str("en-US"), &options);
        // An unknown zone throws a RangeError, caught here.
        let format = Reflect::construct(&constructor, &args).ok()?;
        Some(Self {
            format: format.unchecked_into(),
        })
    }
}

impl Zone for Named {
    fn offset(&self, utc: i64) -> i64 {
        let date = Date::new(&JsValue::from_f64(utc as f64 * 1000.0));
        let parts = self.format.format_to_parts(&date);
        let mut fields = [0i64; 6];
        for part in parts.iter() {
            let kind = Reflect::get(&part, &JsValue::from_str("type"))
                .ok()
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            let value: i64 = Reflect::get(&part, &JsValue::from_str("value"))
                .ok()
                .and_then(|v| v.as_string())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let index = match kind.as_str() {
                "year" => 0,
                "month" => 1,
                "day" => 2,
                "hour" => 3,
                "minute" => 4,
                "second" => 5,
                _ => continue,
            };
            fields[index] = value;
        }
        let [year, month, day, hour, minute, second] = fields;
        let local = days_from_civil(year, month as u32, day as u32) * 86_400
            + (hour % 24) * 3600
            + minute * 60
            + second;
        local - utc
    }
}

/// Now, in Unix seconds.
pub fn now() -> i64 {
    (Date::now() / 1000.0) as i64
}
