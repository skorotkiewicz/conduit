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
    pub models: Option<Vec<String>>,
    pub http_port: Option<u16>,
    pub p2p_port: Option<u16>,
    pub bootstrap_nodes: Option<Vec<String>>,
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

    pub fn validate_request(
        &self,
        request_timestamps: &std::sync::Mutex<Vec<std::time::Instant>>,
        context_length: u32,
    ) -> Result<(), String> {
        // 1. Schedule Check
        if !self.is_within_schedule() {
            return Err("Provider is currently outside of usage hours.".to_string());
        }

        // 2. Rate Limit Check
        if let Some(ref rate_limit) = self.rate_limit {
            let mut timestamps = request_timestamps.lock().unwrap();
            let now = std::time::Instant::now();
            timestamps.retain(|t| now.duration_since(*t).as_secs() < 60);

            if timestamps.len() >= rate_limit.requests_per_minute as usize {
                return Err("Provider rate limit exceeded.".to_string());
            }
            timestamps.push(now);
        }

        // 3. Max Context Check
        if let Some(max_ctx) = self.max_context {
            if context_length > max_ctx {
                return Err(format!("Provider max context exceeded. Received {}, Max {}", context_length, max_ctx));
            }
        }

        Ok(())
    }
}
