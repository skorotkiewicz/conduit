use serde::{Deserialize, Serialize};
use chrono::{NaiveTime, Local};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub max_context: Option<u32>,
    pub rate_limit: Option<RateLimitConfig>,
    pub schedule: Option<ScheduleConfig>,
    pub local_llm: Option<String>,
}

impl ProviderConfig {
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let content = fs::read_to_string(path)?;
        let config: ProviderConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    pub fn is_within_schedule(&self) -> bool {
        if let Some(ref sched) = self.schedule {
            let start = NaiveTime::parse_from_str(&sched.start, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(0, 0, 0).unwrap());
            let end = NaiveTime::parse_from_str(&sched.end, "%H:%M").unwrap_or(NaiveTime::from_hms_opt(23, 59, 59).unwrap());
            
            let now = Local::now().time();
            
            // Handle overnight schedules (e.g. 22:00 to 07:00)
            if start <= end {
                now >= start && now <= end
            } else {
                now >= start || now <= end
            }
        } else {
            true // No schedule means always available
        }
    }
}
