use axum::{
    routing::{get, post, delete},
    Router,
};
use std::env;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rusqlite::Connection;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

pub mod metrics;
pub mod alerts;
pub mod dashboard;
pub mod web;

pub struct AppState {
    pub ups_name: String,
    pub ups_host: String,
    pub fcm_config: Option<FcmConfig>,
    pub last_alerts: Mutex<LastAlertState>,
    pub db_conn: Mutex<Connection>,
}

pub struct FcmConfig {
    pub project_id: String,
    pub client_email: String,
    pub private_key: String,
}

#[derive(Default)]
pub struct LastAlertState {
    pub last_status: String,
    pub battery_low_sent: bool,
    pub load_high_sent: bool,
    pub runtime_low_sent: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nut_monitor=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Initializing NUT Monitor Server...");

    let ups_name = env::var("UPS_NAME").unwrap_or_else(|_| "ups".to_string());
    let ups_host = env::var("UPS_HOST").unwrap_or_else(|_| "localhost".to_string());
    
    let interval_secs = env::var("MONITOR_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(10);

    let fcm_config = if let (Ok(pid), Ok(email), Ok(key)) = (
        env::var("FCM_PROJECT_ID"), env::var("FCM_CLIENT_EMAIL"), env::var("FCM_PRIVATE_KEY")
    ) {
        info!("FCM configuration loaded successfully for project: {}", pid);
        Some(FcmConfig {
            project_id: pid,
            client_email: email,
            private_key: key.replace("\\n", "\n"),
        })
    } else {
        warn!("FCM environment variables missing. Notifications will be disabled.");
        None
    };

    let db_path = env::var("DATABASE_PATH").unwrap_or_else(|_| "/data/devices.db".to_string());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("Could not create database directory {}: {}", parent.display(), e);
        }
    }
    let conn = Connection::open(&db_path).expect("Failed to open database");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS devices (
            device_id TEXT PRIMARY KEY,
            device_name TEXT NOT NULL,
            device_token TEXT NOT NULL UNIQUE
        )",
        [],
    ).expect("Failed to create devices table");

    let shared_state = Arc::new(AppState {
        ups_name,
        ups_host,
        fcm_config,
        last_alerts: Mutex::new(LastAlertState::default()),
        db_conn: Mutex::new(conn),
    });

    let monitor_state = shared_state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        info!("Background monitoring loop started. Interval: {}s", interval_secs);
        loop {
            interval.tick().await;
            alerts::evaluate_alerts(&monitor_state).await;
        }
    });

    let app = Router::new()
        .route("/", get(dashboard::html_handler))
        .route("/api/status", get(web::json_handler))
        .route("/api/register", post(web::register_device_handler))
        .route("/api/test-fcm", post(web::test_fcm_handler))
        .route("/api/devices", get(web::get_devices_handler))
        .route("/api/devices/:id", delete(web::delete_device_handler))
        .layer(TraceLayer::new_for_http())
        .with_state(shared_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!("NUT Monitor server running on http://0.0.0.0:3000");
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute(
            "CREATE TABLE devices (
                device_id TEXT PRIMARY KEY,
                device_name TEXT NOT NULL,
                device_token TEXT NOT NULL UNIQUE
            )",
            [],
        ).unwrap();
        conn
    }

    #[test]
    fn test_device_registration_insert_or_replace() {
        let db = setup_test_db();
        
        // 1. Insert initial device
        db.execute(
            "INSERT OR REPLACE INTO devices (device_id, device_name, device_token) VALUES (?1, ?2, ?3)",
            rusqlite::params!["dev1", "Device One", "token1"],
        ).unwrap();

        // Check count
        let count: i64 = db.query_row("SELECT count(*) FROM devices", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);

        // 2. Register same device_id with a different token (update token)
        db.execute(
            "INSERT OR REPLACE INTO devices (device_id, device_name, device_token) VALUES (?1, ?2, ?3)",
            rusqlite::params!["dev1", "Device One Updated", "token2"],
        ).unwrap();
        
        let count: i64 = db.query_row("SELECT count(*) FROM devices", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let token: String = db.query_row("SELECT device_token FROM devices WHERE device_id = 'dev1'", [], |r| r.get(0)).unwrap();
        assert_eq!(token, "token2");

        // 3. Register a new device_id with the same token (token2)
        db.execute(
            "INSERT OR REPLACE INTO devices (device_id, device_name, device_token) VALUES (?1, ?2, ?3)",
            rusqlite::params!["dev2", "Device Two", "token2"],
        ).unwrap();

        let count: i64 = db.query_row("SELECT count(*) FROM devices", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let id: String = db.query_row("SELECT device_id FROM devices WHERE device_token = 'token2'", [], |r| r.get(0)).unwrap();
        assert_eq!(id, "dev2");
    }

    #[test]
    fn test_metrics_parsing_and_calculation() {
        let stdout = "\
device.mfr: APC
device.model: Back-UPS ES 700
device.serial: 123456789
battery.charge: 95
battery.voltage: 13.6
battery.voltage.nominal: 12
input.voltage: 230
input.voltage.nominal: 230
ups.load: 25.4
ups.realpower.nominal: 405
ups.status: OL
battery.runtime: 1800
";
        let metrics = metrics::parse_upsc_output(stdout);
        assert_eq!(metrics.manufacturer, "APC");
        assert_eq!(metrics.model, "Back-UPS ES 700");
        assert_eq!(metrics.serial, "123456789");
        assert_eq!(metrics.battery_charge, "95");
        assert_eq!(metrics.battery_voltage, "13.6");
        assert_eq!(metrics.battery_voltage_nominal, "12");
        assert_eq!(metrics.input_voltage, "230");
        assert_eq!(metrics.input_voltage_nominal, "230");
        assert_eq!(metrics.ups_load, "25.4");
        assert_eq!(metrics.ups_load_watt, "102.9");
        assert_eq!(metrics.status, "Online (AC)");
        assert_eq!(metrics.runtime_seconds, "1800");
        assert_eq!(metrics.runtime_formatted, "30 min");
    }
}