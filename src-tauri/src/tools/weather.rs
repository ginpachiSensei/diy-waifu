//! Current weather and a 3-day forecast from Open-Meteo (free, no key).

use serde::Deserialize;
use std::time::Duration;

#[derive(Deserialize)]
struct GeoResponse {
    #[serde(default)]
    results: Vec<Place>,
}

#[derive(Deserialize)]
struct Place {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    country: String,
    #[serde(default)]
    admin1: String,
}

#[derive(Deserialize)]
struct Forecast {
    current: Current,
    daily: Daily,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: f64,
    apparent_temperature: f64,
    relative_humidity_2m: f64,
    wind_speed_10m: f64,
    weather_code: u32,
}

#[derive(Deserialize)]
struct Daily {
    time: Vec<String>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    precipitation_probability_max: Vec<Option<f64>>,
    weather_code: Vec<u32>,
}

pub fn describe(code: u32) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51..=57 => "drizzle",
        61..=67 => "rain",
        71..=77 => "snow",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95..=99 => "thunderstorm",
        _ => "unknown conditions",
    }
}

pub struct Weather {
    pub place: String,
    pub text: String,
}

pub async fn weather(place: &str) -> Result<Weather, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;

    let geo: GeoResponse = client
        .get("https://geocoding-api.open-meteo.com/v1/search")
        .query(&[("name", place.trim()), ("count", "1"), ("language", "en")])
        .send()
        .await
        .map_err(|e| format!("Weather lookup failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Weather lookup failed: {e}"))?;
    let p = geo
        .results
        .into_iter()
        .next()
        .ok_or_else(|| format!("Couldn't find a place called \"{place}\""))?;

    let f: Forecast = client
        .get("https://api.open-meteo.com/v1/forecast")
        .query(&[
            ("latitude", p.latitude.to_string()),
            ("longitude", p.longitude.to_string()),
            (
                "current",
                "temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code".into(),
            ),
            (
                "daily",
                "temperature_2m_max,temperature_2m_min,precipitation_probability_max,weather_code".into(),
            ),
            ("forecast_days", "3".into()),
            ("timezone", "auto".into()),
        ])
        .send()
        .await
        .map_err(|e| format!("Weather lookup failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("Weather lookup failed: {e}"))?;

    let name = [p.name.as_str(), p.admin1.as_str(), p.country.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    let c = &f.current;
    let mut text = format!(
        "Weather in {name} now: {}, {:.0}°C (feels like {:.0}°C), humidity {:.0}%, wind {:.0} km/h.",
        describe(c.weather_code),
        c.temperature_2m,
        c.apparent_temperature,
        c.relative_humidity_2m,
        c.wind_speed_10m
    );
    let d = &f.daily;
    for i in 0..d.time.len().min(3) {
        let rain = d.precipitation_probability_max.get(i).copied().flatten().unwrap_or(0.0);
        text.push_str(&format!(
            "\n{}: {}, {:.0}–{:.0}°C, {:.0}% chance of rain.",
            d.time[i],
            describe(d.weather_code.get(i).copied().unwrap_or(999)),
            d.temperature_2m_min.get(i).copied().unwrap_or(f64::NAN),
            d.temperature_2m_max.get(i).copied().unwrap_or(f64::NAN),
            rain
        ));
    }
    Ok(Weather { place: name, text })
}

#[cfg(test)]
mod tests {
    #[test]
    fn describes_codes() {
        assert_eq!(super::describe(0), "clear sky");
        assert_eq!(super::describe(63), "rain");
        assert_eq!(super::describe(1234), "unknown conditions");
    }
}
